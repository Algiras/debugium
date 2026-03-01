"""Task scheduler that processes jobs with priorities."""

from dataclasses import dataclass, field
from typing import Callable


@dataclass
class Job:
    name: str
    priority: int          # higher = more urgent
    fn: Callable
    args: tuple = field(default_factory=tuple)
    result: object = None
    done: bool = False

    def run(self):
        self.result = self.fn(*self.args)
        self.done = True
        return self.result


class Scheduler:
    def __init__(self):
        self._queue: list[Job] = []
        self._completed: list[Job] = []

    def submit(self, job: Job):
        self._queue.append(job)
        # keep sorted by priority descending
        self._queue.sort(key=lambda j: j.priority, reverse=True)

    def run_next(self) -> Job | None:
        if not self._queue:
            return None
        job = self._queue.pop(0)
        job.run()
        self._completed.append(job)
        return job

    def run_all(self) -> list[Job]:
        while self._queue:
            self.run_next()
        return self._completed

    def pending(self) -> list[str]:
        return [j.name for j in self._queue]

    def results(self) -> dict:
        return {j.name: j.result for j in self._completed}
