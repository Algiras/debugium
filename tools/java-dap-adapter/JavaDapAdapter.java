import com.sun.jdi.*;
import com.sun.jdi.connect.*;
import com.sun.jdi.event.*;
import com.sun.jdi.request.*;
import com.google.gson.*;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Minimal standalone Java DAP adapter using JDI.
 * Speaks Debug Adapter Protocol over stdin/stdout.
 */
public class JavaDapAdapter {
    private static final Gson gson = new GsonBuilder().create();
    private static final AtomicInteger seqCounter = new AtomicInteger(1);

    private VirtualMachine vm;
    private Process vmProcess;
    private final Map<String, List<int[]>> breakpoints = new ConcurrentHashMap<>();
    // variablesReference -> list of Variable-like entries
    private final Map<Integer, List<JsonObject>> variableScopes = new ConcurrentHashMap<>();
    private final AtomicInteger nextVarRef = new AtomicInteger(1);
    private ThreadReference stoppedThread;
    private StackFrame currentFrame;
    private String mainClass;
    private String cwd;
    private boolean configDone = false;
    private final Object configLock = new Object();
    private PrintStream errLog;

    public static void main(String[] args) throws Exception {
        new JavaDapAdapter().run();
    }

    private void run() throws Exception {
        errLog = System.err;
        InputStream in = System.in;
        OutputStream out = System.out;
        // Redirect stdout to stderr for debug logging — DAP uses stdout
        System.setOut(new PrintStream(System.err, true));

        BufferedInputStream bis = new BufferedInputStream(in);

        while (true) {
            String message = readDapMessage(bis);
            if (message == null) break;

            JsonObject request = JsonParser.parseString(message).getAsJsonObject();
            String command = request.get("command").getAsString();
            int seq = request.get("seq").getAsInt();
            JsonObject arguments;
            if (request.has("arguments") && request.get("arguments").isJsonObject()) {
                arguments = request.getAsJsonObject("arguments");
            } else {
                arguments = new JsonObject();
            }

            errLog.println("[DAP] <- " + command);

            try {
                switch (command) {
                    case "initialize": handleInitialize(out, seq, arguments); break;
                    case "launch": handleLaunch(out, seq, arguments); break;
                    case "setBreakpoints": handleSetBreakpoints(out, seq, arguments); break;
                    case "configurationDone": handleConfigurationDone(out, seq); break;
                    case "threads": handleThreads(out, seq); break;
                    case "stackTrace": handleStackTrace(out, seq, arguments); break;
                    case "scopes": handleScopes(out, seq, arguments); break;
                    case "variables": handleVariables(out, seq, arguments); break;
                    case "continue": handleContinue(out, seq, arguments); break;
                    case "next": handleNext(out, seq, arguments); break;
                    case "stepIn": handleStepIn(out, seq, arguments); break;
                    case "stepOut": handleStepOut(out, seq, arguments); break;
                    case "evaluate": handleEvaluate(out, seq, arguments); break;
                    case "disconnect": handleDisconnect(out, seq); return;
                    default:
                        sendResponse(out, seq, command, true, new JsonObject());
                        break;
                }
            } catch (Exception e) {
                errLog.println("[DAP] Error handling " + command + ": " + e.getMessage());
                e.printStackTrace(errLog);
                JsonObject body = new JsonObject();
                sendErrorResponse(out, seq, command, e.getMessage());
            }
        }
    }

    private void handleInitialize(OutputStream out, int seq, JsonObject args) throws IOException {
        JsonObject caps = new JsonObject();
        caps.addProperty("supportsConfigurationDoneRequest", true);
        caps.addProperty("supportsFunctionBreakpoints", false);
        caps.addProperty("supportsConditionalBreakpoints", false);
        caps.addProperty("supportsEvaluateForHovers", true);
        caps.addProperty("supportsSetVariable", false);
        caps.addProperty("supportsExceptionInfoRequest", false);
        sendResponse(out, seq, "initialize", true, caps);
        sendEvent(out, "initialized", new JsonObject());
    }

    private void handleLaunch(OutputStream out, int seq, JsonObject args) throws Exception {
        // Debugium sends "program" via DapConfig path, "mainClass" via built-in Java path
        mainClass = args.has("mainClass") ? args.get("mainClass").getAsString()
                  : args.has("program") ? args.get("program").getAsString() : "";
        cwd = args.has("cwd") ? args.get("cwd").getAsString() : ".";

        // Use LaunchingConnector to start the target VM
        LaunchingConnector connector = Bootstrap.virtualMachineManager().defaultConnector();
        Map<String, Connector.Argument> connArgs = connector.defaultArguments();
        // Derive classpath from mainClass directory (e.g., /tmp/TargetJava -> /tmp)
        String classpath = cwd;
        if (mainClass.contains("/")) {
            classpath = mainClass.substring(0, mainClass.lastIndexOf('/'));
            mainClass = mainClass.substring(mainClass.lastIndexOf('/') + 1);
        }
        // Allow explicit classpath from launch config (e.g. for Scala library JARs)
        if (args.has("classpath") && !args.get("classpath").getAsString().isEmpty()) {
            classpath = args.get("classpath").getAsString();
        }
        connArgs.get("main").setValue(mainClass);
        if (connArgs.containsKey("options")) {
            connArgs.get("options").setValue("-cp " + classpath);
        }

        errLog.println("[DAP] Launching: " + mainClass + " in " + cwd);
        vm = connector.launch(connArgs);
        vmProcess = vm.process();

        // Forward VM stdout/stderr to DAP output events
        startOutputForwarder(vmProcess.getInputStream(), "stdout", out);
        startOutputForwarder(vmProcess.getErrorStream(), "stderr", out);

        // Start event processing thread
        startEventProcessor(out);

        sendResponse(out, seq, "launch", true, new JsonObject());
    }

    private void handleSetBreakpoints(OutputStream out, int seq, JsonObject args) throws Exception {
        JsonObject source = args.getAsJsonObject("source");
        String path = source.has("path") ? source.get("path").getAsString() : "";
        JsonArray bpArray = args.has("breakpoints") ? args.getAsJsonArray("breakpoints") : new JsonArray();

        JsonArray resultBps = new JsonArray();

        // Clear existing breakpoints for this file
        if (vm != null) {
            EventRequestManager erm = vm.eventRequestManager();
            List<BreakpointRequest> toDelete = new ArrayList<>();
            for (BreakpointRequest br : erm.breakpointRequests()) {
                Location loc = br.location();
                try {
                    String srcName = loc.sourcePath();
                    if (path.endsWith(srcName)) {
                        toDelete.add(br);
                    }
                } catch (AbsentInformationException e) { /* skip */ }
            }
            for (BreakpointRequest br : toDelete) {
                erm.deleteEventRequest(br);
            }
        }

        // Derive class name from file path
        String className = deriveClassName(path);

        for (JsonElement bpEl : bpArray) {
            JsonObject bp = bpEl.getAsJsonObject();
            int line = bp.get("line").getAsInt();

            JsonObject resultBp = new JsonObject();
            resultBp.addProperty("line", line);
            resultBp.addProperty("verified", false);

            if (vm != null && className != null) {
                try {
                    // Try className and className$ (Scala companion object)
                    boolean bpSet = false;
                    for (String cn : new String[]{className, className + "$"}) {
                        List<ReferenceType> classes = vm.classesByName(cn);
                        if (!classes.isEmpty()) {
                            ReferenceType refType = classes.get(0);
                            List<Location> locations = refType.locationsOfLine(line);
                            if (!locations.isEmpty()) {
                                BreakpointRequest bpReq = vm.eventRequestManager().createBreakpointRequest(locations.get(0));
                                bpReq.setSuspendPolicy(EventRequest.SUSPEND_ALL);
                                bpReq.enable();
                                resultBp.addProperty("verified", true);
                                errLog.println("[DAP] Breakpoint set at " + cn + ":" + line);
                                bpSet = true;
                                break;
                            }
                        }
                    }
                    if (!bpSet && vm.classesByName(className).isEmpty()) {
                        // Class not loaded yet, set deferred breakpoint via ClassPrepare
                        errLog.println("[DAP] Class " + className + " not loaded, deferring breakpoint at line " + line);
                        ClassPrepareRequest cpr = vm.eventRequestManager().createClassPrepareRequest();
                        cpr.addClassFilter(className);
                        cpr.setSuspendPolicy(EventRequest.SUSPEND_ALL);
                        cpr.enable();
                        // Store pending breakpoints
                        breakpoints.computeIfAbsent(className, k -> new ArrayList<>())
                            .add(new int[]{line, seq});

                        // Also register for Scala companion object class (ClassName$)
                        String scalaCompanion = className + "$";
                        ClassPrepareRequest cpr2 = vm.eventRequestManager().createClassPrepareRequest();
                        cpr2.addClassFilter(scalaCompanion);
                        cpr2.setSuspendPolicy(EventRequest.SUSPEND_ALL);
                        cpr2.enable();
                        breakpoints.computeIfAbsent(scalaCompanion, k -> new ArrayList<>())
                            .add(new int[]{line, seq});
                        errLog.println("[DAP] Also deferring for Scala companion: " + scalaCompanion);
                    }
                } catch (Exception e) {
                    errLog.println("[DAP] Failed to set breakpoint: " + e.getMessage());
                }
            }

            JsonObject bpSource = new JsonObject();
            bpSource.addProperty("path", path);
            resultBp.add("source", bpSource);
            resultBps.add(resultBp);
        }

        JsonObject body = new JsonObject();
        body.add("breakpoints", resultBps);
        sendResponse(out, seq, "setBreakpoints", true, body);
    }

    private void handleConfigurationDone(OutputStream out, int seq) throws IOException {
        synchronized (configLock) {
            configDone = true;
            configLock.notifyAll();
        }
        sendResponse(out, seq, "configurationDone", true, new JsonObject());
    }

    private void handleThreads(OutputStream out, int seq) throws IOException {
        JsonArray threads = new JsonArray();
        if (vm != null) {
            try {
                for (ThreadReference t : vm.allThreads()) {
                    JsonObject thread = new JsonObject();
                    thread.addProperty("id", (int) t.uniqueID());
                    thread.addProperty("name", t.name());
                    threads.add(thread);
                }
            } catch (VMDisconnectedException e) { /* empty */ }
        }
        JsonObject body = new JsonObject();
        body.add("threads", threads);
        sendResponse(out, seq, "threads", true, body);
    }

    private void handleStackTrace(OutputStream out, int seq, JsonObject args) throws Exception {
        long threadId = args.get("threadId").getAsLong();
        JsonArray frames = new JsonArray();

        if (vm != null) {
            for (ThreadReference t : vm.allThreads()) {
                if (t.uniqueID() == threadId) {
                    try {
                        int frameId = 0;
                        for (StackFrame frame : t.frames()) {
                            Location loc = frame.location();
                            JsonObject f = new JsonObject();
                            f.addProperty("id", frameId);
                            f.addProperty("name", loc.method().name());
                            f.addProperty("line", loc.lineNumber());
                            f.addProperty("column", 0);

                            JsonObject source = new JsonObject();
                            try {
                                source.addProperty("path", loc.sourcePath());
                                source.addProperty("name", loc.sourceName());
                            } catch (AbsentInformationException e) {
                                source.addProperty("name", loc.declaringType().name());
                            }
                            f.add("source", source);
                            frames.add(f);
                            frameId++;
                        }
                    } catch (IncompatibleThreadStateException e) {
                        errLog.println("[DAP] Thread not suspended: " + e.getMessage());
                    }
                    break;
                }
            }
        }

        JsonObject body = new JsonObject();
        body.add("stackFrames", frames);
        body.addProperty("totalFrames", frames.size());
        sendResponse(out, seq, "stackTrace", true, body);
    }

    private void handleScopes(OutputStream out, int seq, JsonObject args) throws Exception {
        int frameId = args.get("frameId").getAsInt();
        JsonArray scopes = new JsonArray();

        if (stoppedThread != null) {
            try {
                List<StackFrame> frames = stoppedThread.frames();
                if (frameId < frames.size()) {
                    currentFrame = frames.get(frameId);

                    // Locals scope
                    int localRef = nextVarRef.getAndIncrement();
                    JsonObject localScope = new JsonObject();
                    localScope.addProperty("name", "Locals");
                    localScope.addProperty("variablesReference", localRef);
                    localScope.addProperty("expensive", false);
                    scopes.add(localScope);

                    // Build variable list for this scope
                    List<JsonObject> vars = new ArrayList<>();
                    try {
                        for (LocalVariable lv : currentFrame.visibleVariables()) {
                            Value val = currentFrame.getValue(lv);
                            JsonObject v = new JsonObject();
                            v.addProperty("name", lv.name());
                            v.addProperty("value", valueToString(val));
                            v.addProperty("type", lv.typeName());
                            v.addProperty("variablesReference", getChildRef(val));
                            vars.add(v);
                        }
                    } catch (AbsentInformationException e) {
                        // compiled without -g
                    }

                    // Add 'this' if available
                    try {
                        ObjectReference thisObj = currentFrame.thisObject();
                        if (thisObj != null) {
                            JsonObject v = new JsonObject();
                            v.addProperty("name", "this");
                            v.addProperty("value", thisObj.toString());
                            v.addProperty("type", thisObj.referenceType().name());
                            v.addProperty("variablesReference", getChildRef(thisObj));
                            vars.add(v);
                        }
                    } catch (Exception e) { /* static method */ }

                    variableScopes.put(localRef, vars);
                }
            } catch (IncompatibleThreadStateException e) {
                errLog.println("[DAP] Thread not suspended for scopes");
            }
        }

        JsonObject body = new JsonObject();
        body.add("scopes", scopes);
        sendResponse(out, seq, "scopes", true, body);
    }

    private void handleVariables(OutputStream out, int seq, JsonObject args) throws IOException {
        int ref = args.get("variablesReference").getAsInt();
        JsonArray variables = new JsonArray();

        List<JsonObject> vars = variableScopes.get(ref);
        if (vars != null) {
            for (JsonObject v : vars) {
                variables.add(v);
            }
        }

        JsonObject body = new JsonObject();
        body.add("variables", variables);
        sendResponse(out, seq, "variables", true, body);
    }

    private void handleContinue(OutputStream out, int seq, JsonObject args) throws IOException {
        if (vm != null) {
            vm.resume();
        }
        JsonObject body = new JsonObject();
        body.addProperty("allThreadsContinued", true);
        sendResponse(out, seq, "continue", true, body);
    }

    private void handleNext(OutputStream out, int seq, JsonObject args) throws Exception {
        long threadId = args.get("threadId").getAsLong();
        if (vm != null) {
            for (ThreadReference t : vm.allThreads()) {
                if (t.uniqueID() == threadId) {
                    StepRequest sr = vm.eventRequestManager().createStepRequest(
                        t, StepRequest.STEP_LINE, StepRequest.STEP_OVER);
                    sr.addCountFilter(1);
                    sr.setSuspendPolicy(EventRequest.SUSPEND_ALL);
                    sr.enable();
                    vm.resume();
                    break;
                }
            }
        }
        sendResponse(out, seq, "next", true, new JsonObject());
    }

    private void handleStepIn(OutputStream out, int seq, JsonObject args) throws Exception {
        long threadId = args.get("threadId").getAsLong();
        if (vm != null) {
            for (ThreadReference t : vm.allThreads()) {
                if (t.uniqueID() == threadId) {
                    StepRequest sr = vm.eventRequestManager().createStepRequest(
                        t, StepRequest.STEP_LINE, StepRequest.STEP_INTO);
                    sr.addCountFilter(1);
                    sr.setSuspendPolicy(EventRequest.SUSPEND_ALL);
                    sr.enable();
                    vm.resume();
                    break;
                }
            }
        }
        sendResponse(out, seq, "stepIn", true, new JsonObject());
    }

    private void handleStepOut(OutputStream out, int seq, JsonObject args) throws Exception {
        long threadId = args.get("threadId").getAsLong();
        if (vm != null) {
            for (ThreadReference t : vm.allThreads()) {
                if (t.uniqueID() == threadId) {
                    StepRequest sr = vm.eventRequestManager().createStepRequest(
                        t, StepRequest.STEP_LINE, StepRequest.STEP_OUT);
                    sr.addCountFilter(1);
                    sr.setSuspendPolicy(EventRequest.SUSPEND_ALL);
                    sr.enable();
                    vm.resume();
                    break;
                }
            }
        }
        sendResponse(out, seq, "stepOut", true, new JsonObject());
    }

    private void handleEvaluate(OutputStream out, int seq, JsonObject args) throws Exception {
        String expression = args.get("expression").getAsString();
        String result = "?";

        if (stoppedThread != null && currentFrame != null) {
            try {
                // Try to find as a local variable
                for (LocalVariable lv : currentFrame.visibleVariables()) {
                    if (lv.name().equals(expression)) {
                        Value val = currentFrame.getValue(lv);
                        result = valueToString(val);
                        break;
                    }
                }
            } catch (Exception e) {
                result = "Error: " + e.getMessage();
            }
        }

        JsonObject body = new JsonObject();
        body.addProperty("result", result);
        body.addProperty("variablesReference", 0);
        sendResponse(out, seq, "evaluate", true, body);
    }

    private void handleDisconnect(OutputStream out, int seq) throws IOException {
        if (vm != null) {
            try {
                vm.exit(0);
            } catch (Exception e) { /* already dead */ }
        }
        sendResponse(out, seq, "disconnect", true, new JsonObject());
        sendEvent(out, "terminated", new JsonObject());
    }

    // --- Event Processing ---

    private void startEventProcessor(OutputStream out) {
        Thread t = new Thread(() -> {
            // Wait for configurationDone
            synchronized (configLock) {
                while (!configDone) {
                    try { configLock.wait(5000); } catch (InterruptedException e) { break; }
                }
            }

            EventQueue queue = vm.eventQueue();
            while (true) {
                try {
                    EventSet eventSet = queue.remove();
                    for (Event event : eventSet) {
                        errLog.println("[DAP] VM event: " + event.getClass().getSimpleName());

                        if (event instanceof ClassPrepareEvent) {
                            ClassPrepareEvent cpe = (ClassPrepareEvent) event;
                            String className = cpe.referenceType().name();
                            List<int[]> pending = breakpoints.get(className);
                            if (pending != null) {
                                for (int[] bp : pending) {
                                    try {
                                        List<Location> locs = cpe.referenceType().locationsOfLine(bp[0]);
                                        if (!locs.isEmpty()) {
                                            BreakpointRequest bpReq = vm.eventRequestManager()
                                                .createBreakpointRequest(locs.get(0));
                                            bpReq.setSuspendPolicy(EventRequest.SUSPEND_ALL);
                                            bpReq.enable();
                                            errLog.println("[DAP] Deferred breakpoint resolved: " + className + ":" + bp[0]);

                                            // Send breakpoint verified event
                                            JsonObject bpBody = new JsonObject();
                                            JsonObject bpObj = new JsonObject();
                                            bpObj.addProperty("verified", true);
                                            bpObj.addProperty("line", bp[0]);
                                            bpObj.addProperty("reason", "changed");
                                            bpBody.add("breakpoint", bpObj);
                                            sendEvent(out, "breakpoint", bpBody);
                                        }
                                    } catch (Exception e) {
                                        errLog.println("[DAP] Failed to resolve deferred bp: " + e.getMessage());
                                    }
                                }
                                breakpoints.remove(className);
                            }
                            // Delete the ClassPrepareRequest
                            vm.eventRequestManager().deleteEventRequest(event.request());
                            eventSet.resume();

                        } else if (event instanceof BreakpointEvent) {
                            BreakpointEvent bpe = (BreakpointEvent) event;
                            stoppedThread = bpe.thread();
                            errLog.println("[DAP] Breakpoint hit at " + bpe.location());

                            JsonObject body = new JsonObject();
                            body.addProperty("reason", "breakpoint");
                            body.addProperty("threadId", (int) bpe.thread().uniqueID());
                            body.addProperty("allThreadsStopped", true);
                            try { sendEvent(out, "stopped", body); } catch (IOException ex) { errLog.println("[DAP] IO error: " + ex); }
                            // Do NOT resume — keep suspended

                        } else if (event instanceof StepEvent) {
                            StepEvent se = (StepEvent) event;
                            stoppedThread = se.thread();
                            // Delete the step request
                            vm.eventRequestManager().deleteEventRequest(se.request());

                            JsonObject body = new JsonObject();
                            body.addProperty("reason", "step");
                            body.addProperty("threadId", (int) se.thread().uniqueID());
                            body.addProperty("allThreadsStopped", true);
                            try { sendEvent(out, "stopped", body); } catch (IOException ex) { errLog.println("[DAP] IO error: " + ex); }

                        } else if (event instanceof VMDeathEvent || event instanceof VMDisconnectEvent) {
                            JsonObject body = new JsonObject();
                            try { sendEvent(out, "terminated", body); } catch (IOException ex) { /* ignore */ }
                            return;
                        } else {
                            eventSet.resume();
                        }
                    }
                } catch (InterruptedException e) {
                    break;
                } catch (VMDisconnectedException e) {
                    try {
                        sendEvent(out, "terminated", new JsonObject());
                    } catch (IOException ex) { /* ignore */ }
                    return;
                }
            }
        }, "dap-event-processor");
        t.setDaemon(true);
        t.start();
    }

    private void startOutputForwarder(InputStream is, String category, OutputStream dapOut) {
        Thread t = new Thread(() -> {
            try (BufferedReader br = new BufferedReader(new InputStreamReader(is))) {
                String line;
                while ((line = br.readLine()) != null) {
                    JsonObject body = new JsonObject();
                    body.addProperty("category", category);
                    body.addProperty("output", line + "\n");
                    sendEvent(dapOut, "output", body);
                }
            } catch (IOException e) { /* stream closed */ }
        }, "output-" + category);
        t.setDaemon(true);
        t.start();
    }

    // --- Helpers ---

    private String deriveClassName(String path) {
        if (path == null || path.isEmpty()) return null;
        // Extract class name from file path: /path/to/TargetJava.java -> TargetJava
        String name = path;
        int lastSlash = name.lastIndexOf('/');
        if (lastSlash >= 0) name = name.substring(lastSlash + 1);
        if (name.endsWith(".java")) name = name.substring(0, name.length() - 5);
        if (name.endsWith(".scala")) name = name.substring(0, name.length() - 6);
        return name;
    }

    private String valueToString(Value val) {
        if (val == null) return "null";
        if (val instanceof StringReference) return "\"" + ((StringReference) val).value() + "\"";
        if (val instanceof PrimitiveValue) return val.toString();
        if (val instanceof ArrayReference) {
            ArrayReference arr = (ArrayReference) val;
            return arr.type().name() + "[" + arr.length() + "]";
        }
        if (val instanceof ObjectReference) {
            ObjectReference obj = (ObjectReference) val;
            // Try toString()
            try {
                return obj.referenceType().name() + "@" + obj.uniqueID();
            } catch (Exception e) { return val.toString(); }
        }
        return val.toString();
    }

    private int getChildRef(Value val) {
        if (val instanceof ObjectReference && !(val instanceof StringReference)) {
            ObjectReference obj = (ObjectReference) val;
            int ref = nextVarRef.getAndIncrement();
            List<JsonObject> children = new ArrayList<>();

            if (val instanceof ArrayReference) {
                ArrayReference arr = (ArrayReference) val;
                int i = 0;
                for (Value elem : arr.getValues()) {
                    JsonObject v = new JsonObject();
                    v.addProperty("name", "[" + i + "]");
                    v.addProperty("value", valueToString(elem));
                    v.addProperty("type", elem != null ? elem.type().name() : "null");
                    v.addProperty("variablesReference", getChildRef(elem));
                    children.add(v);
                    i++;
                    if (i > 100) break; // limit
                }
            } else {
                for (Field f : obj.referenceType().allFields()) {
                    Value fVal = obj.getValue(f);
                    JsonObject v = new JsonObject();
                    v.addProperty("name", f.name());
                    v.addProperty("value", valueToString(fVal));
                    v.addProperty("type", f.typeName());
                    v.addProperty("variablesReference", 0); // don't recurse too deep
                    children.add(v);
                }
            }

            variableScopes.put(ref, children);
            return ref;
        }
        return 0;
    }

    // --- DAP Protocol I/O ---

    private String readDapMessage(InputStream in) throws IOException {
        // Read headers
        int contentLength = -1;
        StringBuilder headerLine = new StringBuilder();
        while (true) {
            int b = in.read();
            if (b == -1) return null;
            if (b == '\r') {
                b = in.read(); // consume \n
                if (headerLine.length() == 0) break; // empty line = end of headers
                String header = headerLine.toString();
                if (header.startsWith("Content-Length:")) {
                    contentLength = Integer.parseInt(header.substring(15).trim());
                }
                headerLine.setLength(0);
            } else {
                headerLine.append((char) b);
            }
        }

        if (contentLength <= 0) return null;

        byte[] body = new byte[contentLength];
        int read = 0;
        while (read < contentLength) {
            int n = in.read(body, read, contentLength - read);
            if (n == -1) return null;
            read += n;
        }
        return new String(body, StandardCharsets.UTF_8);
    }

    private synchronized void sendDapMessage(OutputStream out, String json) throws IOException {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        String header = "Content-Length: " + bytes.length + "\r\n\r\n";
        out.write(header.getBytes(StandardCharsets.UTF_8));
        out.write(bytes);
        out.flush();
    }

    private void sendResponse(OutputStream out, int requestSeq, String command, boolean success, JsonObject body) throws IOException {
        JsonObject resp = new JsonObject();
        resp.addProperty("seq", seqCounter.getAndIncrement());
        resp.addProperty("type", "response");
        resp.addProperty("request_seq", requestSeq);
        resp.addProperty("success", success);
        resp.addProperty("command", command);
        resp.add("body", body);
        String json = gson.toJson(resp);
        errLog.println("[DAP] -> response " + command);
        sendDapMessage(out, json);
    }

    private void sendErrorResponse(OutputStream out, int requestSeq, String command, String message) throws IOException {
        JsonObject resp = new JsonObject();
        resp.addProperty("seq", seqCounter.getAndIncrement());
        resp.addProperty("type", "response");
        resp.addProperty("request_seq", requestSeq);
        resp.addProperty("success", false);
        resp.addProperty("command", command);
        resp.addProperty("message", message);
        resp.add("body", new JsonObject());
        sendDapMessage(out, gson.toJson(resp));
    }

    private void sendEvent(OutputStream out, String event, JsonObject body) throws IOException {
        JsonObject evt = new JsonObject();
        evt.addProperty("seq", seqCounter.getAndIncrement());
        evt.addProperty("type", "event");
        evt.addProperty("event", event);
        evt.add("body", body);
        String json = gson.toJson(evt);
        errLog.println("[DAP] -> event " + event);
        sendDapMessage(out, json);
    }
}
