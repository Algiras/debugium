#!/bin/bash
# Wrapper to launch the JavaDapAdapter as a DAP server over stdin/stdout
DIR="$(cd "$(dirname "$0")" && pwd)"
GSON_JAR="/Users/algimantask/Library/Caches/Coursier/v1/https/repo1.maven.org/maven2/com/google/code/gson/gson/2.10.1/gson-2.10.1.jar"
exec java -cp "$DIR:$GSON_JAR" JavaDapAdapter "$@"
