"""Narrow real-Windows replacements for three POSIX REPL fixture assumptions."""

from __future__ import annotations

import ctypes
import ctypes.wintypes as wintypes
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest

from test_repl import ReplProcess, one, reply_ok


class ProcessIdentity:
    """Keep an exact process handle open; PID reuse cannot satisfy an exit check."""

    def __init__(self, pid: int) -> None:
        self.pid = pid
        self.k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        for name, arguments, result in (
            ("OpenProcess", [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD], wintypes.HANDLE),
            ("WaitForSingleObject", [wintypes.HANDLE, wintypes.DWORD], wintypes.DWORD),
            ("TerminateProcess", [wintypes.HANDLE, wintypes.UINT], wintypes.BOOL),
            ("GetProcessTimes", [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4, wintypes.BOOL),
            ("CloseHandle", [wintypes.HANDLE], wintypes.BOOL),
        ):
            function = getattr(self.k32, name)
            function.argtypes, function.restype = arguments, result
        self.handle = self.k32.OpenProcess(0x00101001, False, pid)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        times = [wintypes.FILETIME() for _ in range(4)]
        if not self.k32.GetProcessTimes(self.handle, *(ctypes.byref(value) for value in times)):
            self.close()
            raise ctypes.WinError(ctypes.get_last_error())
        self.created = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime

    def exited(self, timeout: float = 0) -> bool:
        result = self.k32.WaitForSingleObject(self.handle, int(timeout * 1000))
        if result not in (0, 258):
            raise ctypes.WinError(ctypes.get_last_error())
        return result == 0

    def kill(self) -> None:
        if not self.exited() and not self.k32.TerminateProcess(self.handle, 73):
            raise ctypes.WinError(ctypes.get_last_error())

    def close(self) -> None:
        if self.handle:
            self.k32.CloseHandle(self.handle)
            self.handle = None


class ScopedRepl(ReplProcess):
    def __init__(self, directory: Path) -> None:
        source = str(Path(__file__).resolve().parents[1] / "src")
        env = {
            **os.environ,
            "PYTHONPATH": source,
            "PYTHONDONTWRITEBYTECODE": "1",
            "HOME": str(directory),
            "USERPROFILE": str(directory),
            "PRIME_AGENT_DIR": str(directory / "agent"),
            "PRIME_AGENT_KERNEL_OWNER_PID": str(os.getpid()),
            "PRIME_AGENT_INTERNAL_ORPHAN_PROCESS_JOURNAL": str(directory / "orphan-journal.jsonl"),
        }
        self.spawned_at = time.monotonic()
        self.proc = subprocess.Popen(
            [sys.executable, "-m", "rlm.repl"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            env=env,
            cwd=directory,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        self.identities = [ProcessIdentity(self.proc.pid)]
        self.raw_lines = []
        self._lines = queue.Queue()
        threading.Thread(target=self._read_lines, daemon=True).start()
        if self.ready()[0].get("event") != "ready":
            self.close()
            raise AssertionError("REPL ready handshake missing")
        events = self.execute("native-identity", "import os\nos.getpid()")
        runtime_pid = int(one(events, "result")["text"])
        if runtime_pid != self.proc.pid:
            self.identities.append(ProcessIdentity(runtime_pid))

    def until_done(self, rid: str) -> list[dict]:
        events = []
        deadline = time.monotonic() + 15
        while True:
            event = self.read_event(max(0.01, deadline - time.monotonic()))
            events.append(event)
            if event.get("event") == "host_request":
                reply_ok(self, event)
            if event.get("event") == "done" and event.get("id") == rid:
                return events

    def collect_for(self, seconds: float) -> list[dict]:
        events = []
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                event = self.read_event(deadline - time.monotonic())
            except TimeoutError:
                break
            events.append(event)
            if event.get("event") == "host_request":
                reply_ok(self, event)
        return events

    def close(self) -> None:
        for identity in reversed(self.identities):
            try:
                identity.kill()
                if not identity.exited(5):
                    raise AssertionError(f"owned process {identity.pid} did not exit")
                print(json.dumps({"cleanupPid": identity.pid, "created": identity.created, "exited": True}))
            finally:
                identity.close()
        self.identities = []
        self.proc.wait(timeout=5)
        for stream in (self.proc.stdin, self.proc.stdout):
            if stream is not None:
                stream.close()


JOB_TRACE_SETUP = r'''
import ctypes, ctypes.wintypes as wt, importlib, time
_bm = importlib.import_module('rlm.bash')
_rt = importlib.import_module('rlm.repl')
_k32 = ctypes.WinDLL('kernel32', use_last_error=True)
_k32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
_k32.OpenProcess.restype = wt.HANDLE
_k32.WaitForSingleObject.argtypes = [wt.HANDLE, wt.DWORD]
_k32.WaitForSingleObject.restype = wt.DWORD
_k32.CloseHandle.argtypes = [wt.HANDLE]
_k32.CloseHandle.restype = wt.BOOL
_k32.GetProcessTimes.argtypes = [wt.HANDLE] + [ctypes.POINTER(wt.FILETIME)] * 4
_k32.GetProcessTimes.restype = wt.BOOL
class _JobPids(ctypes.Structure):
    _fields_ = [('assigned', wt.DWORD), ('count', wt.DWORD), ('pids', ctypes.c_size_t * 256)]
def _job_pids(job):
    info = _JobPids()
    if not _bm._winjob._kernel32().QueryInformationJobObject(job, 3, ctypes.byref(info), ctypes.sizeof(info), None):
        raise ctypes.WinError(ctypes.get_last_error())
    return list(info.pids[:info.count])
_original_reap = _bm.BashHandle._reap_group
_reap_traces = []
def _traced_reap(handle):
    members = []
    for pid in _job_pids(handle._job):
        native = _k32.OpenProcess(0x00101000, False, pid)
        if not native:
            raise ctypes.WinError(ctypes.get_last_error())
        stamp = [wt.FILETIME() for _ in range(4)]
        if not _k32.GetProcessTimes(native, *(ctypes.byref(v) for v in stamp)):
            raise ctypes.WinError(ctypes.get_last_error())
        members.append((pid, native, (stamp[0].dwHighDateTime << 32) | stamp[0].dwLowDateTime))
    before = {'phase': 'before-reap', 'pid': handle._pid, 'leaderExit': handle._proc.poll(), 'jobEmpty': _bm._winjob.is_empty(handle._job), 'members': [pid for pid, _, _ in members]}
    _rt._send({'event': 'test_trace', **before})
    delivered = _original_reap(handle)
    gone = []
    for pid, native, created in members:
        try:
            gone.append({'pid': pid, 'created': created, 'exited': _k32.WaitForSingleObject(native, 2000) == 0})
        finally:
            _k32.CloseHandle(native)
    after = {'phase': 'after-reap', 'delivered': delivered, 'jobClosed': handle._job is None, 'members': gone}
    _reap_traces.append(after)
    _rt._send({'event': 'test_trace', **after})
    return delivered
_bm.BashHandle._reap_group = _traced_reap
'''


INTERRUPT_TRACE_SETUP = r'''
import importlib, time
_rt = importlib.import_module('rlm.repl')
_original_interrupt = _rt._request_interrupt
_original_signal = _rt._sigint_handler
def _traced_interrupt(target):
    _rt._send({'event': 'test_trace', 'phase': 'interrupt-request', 'target': target, 'finishing': _rt._finishing_rid, 'active': _rt._active['rid']})
    _original_interrupt(target)
    _rt._send({'event': 'test_trace', 'phase': 'interrupt-queued', 'target': _rt._sigint_target})
def _traced_signal(signum, frame):
    _rt._send({'event': 'test_trace', 'phase': 'signal-handler', 'target': _rt._sigint_target, 'finishing': _rt._finishing_rid})
    return _original_signal(signum, frame)
_rt._request_interrupt = _traced_interrupt
_rt._sigint_handler = _traced_signal
'''


@unittest.skipUnless(os.name == "nt", "direct Windows process/signal contract")
class WindowsFinishTest(unittest.TestCase):
    def setUp(self) -> None:
        shell = os.environ.get("PRIME_AGENT_BASH_SHELL", "")
        self.assertTrue(os.path.isabs(shell) and Path(shell).is_file(), "absolute Git Bash injection required")
        self.directory = tempfile.TemporaryDirectory(prefix="prime-finish-")
        self.addCleanup(self.directory.cleanup)
        self.repl = ScopedRepl(Path(self.directory.name))
        self.addCleanup(self.repl.close)

    def _trace_until(self, phase: str, events: list[dict]) -> None:
        deadline = time.monotonic() + 5
        while not any(event.get("phase") == phase for event in events):
            events.append(self.repl.read_event(max(0.01, deadline - time.monotonic())))

    def test_bash_activity_and_notice_wait_for_windows_job_reap(self) -> None:
        self.assertEqual(one(self.repl.execute("job-trace", JOB_TRACE_SETUP), "done")["status"], "ok")
        mime = "application/vnd.prime-agent.bash-activity+json"
        for awaited in (False, True):
            with self.subTest(awaited=awaited):
                events = self.repl.execute(
                    "group-start",
                    "from rlm import bash\nimport asyncio\n"
                    "handle = bash('sleep 30 &')\n"
                    + ("await handle\n" if awaited else "")
                    + "await asyncio.wait_for(handle._wait(), 3)\n"
                    "while not handle._reaped:\n    await asyncio.sleep(0.01)\n"
                    "(handle._group_alive(), handle._reaped)",
                )
                self.assertEqual(one(events, "result")["text"], "(False, True)")
                deadline = time.monotonic() + 5
                while not any(event.get("data", {}).get(mime, {}).get("active") is False for event in events):
                    event = self.repl.read_event(max(0.01, deadline - time.monotonic()))
                    events.append(event)
                    if event.get("event") == "host_request":
                        reply_ok(self.repl, event)
                traces = [event for event in events if event.get("event") == "test_trace"]
                print(json.dumps({"awaited": awaited, "jobTrace": traces}))
                before = next(event for event in traces if event["phase"] == "before-reap")
                after = next(event for event in traces if event["phase"] == "after-reap")
                self.assertEqual(before["leaderExit"], 0)
                self.assertTrue(after["delivered"] and after["jobClosed"])
                self.assertTrue(all(member["exited"] for member in after["members"]))
                self.assertEqual(sum(event.get("event") == "host_request" for event in events), 0 if awaited else 1)
                inactive = [index for index, event in enumerate(events) if event.get("data", {}).get(mime, {}).get("active") is False]
                self.assertEqual(len(inactive), 1)
                self.assertLess(events.index(after), inactive[0])

    def test_finishing_sleep_keeps_interrupt_target_until_authoritative_error(self) -> None:
        self.assertEqual(one(self.repl.execute("interrupt-trace", INTERRUPT_TRACE_SETUP), "done")["status"], "ok")
        self.repl.send({"type": "execute", "id": "slowrepr", "code": "saved = [17]\nclass SlowRepr:\n    def __repr__(self):\n        _rt._send({'event':'test_trace','phase':'repr-start'})\n        time.sleep(2)\n        return 'late'\nSlowRepr()"})
        events: list[dict] = []
        self._trace_until("repr-start", events)
        self.repl.send({"type": "interrupt", "id": "slowrepr"})
        self._trace_until("interrupt-queued", events)
        self.repl.send({"type": "execute", "id": "following", "code": "saved, 1+1"})
        events += self.repl.collect_for(0.3)
        self.assertFalse(any(event.get("event") in ("done", "result") for event in events))
        self.assertFalse(any(event.get("phase") == "signal-handler" for event in events))
        events += self.repl.until_done("following")
        print(json.dumps({"finishingInterruptTrace": events}))
        request = next(event for event in events if event.get("phase") == "interrupt-request")
        self.assertEqual(request["finishing"], "slowrepr")
        self.assertIsNone(request["active"])
        self.assertEqual(sum(event.get("phase") == "interrupt-queued" for event in events), 1)
        self.assertEqual(sum(event.get("phase") == "signal-handler" for event in events), 1)
        self.assertEqual(one(events, "error")["ename"], "KeyboardInterrupt")
        self.assertEqual(one(events, "done")["status"], "error")
        self.assertFalse(any(event.get("event") == "result" and event.get("id") == "slowrepr" for event in events))
        self.assertEqual(one(events, "result")["text"], "([17], 2)")
        self.assertEqual(events[-1]["status"], "ok")

    def test_nonresponsive_repr_explicit_kill_restart_and_legacy_restore(self) -> None:
        self.repl.execute("seed", "saved = {'proof': 31}")
        state = Path(self.directory.name) / "kernel-state.dill"
        self.repl.send({"type": "snapshot", "id": "save", "path": str(state), "manifest_path": str(state.with_suffix(".json")), "snapshot_format": "legacy"})
        self.assertEqual(one(self.repl.until_done("save"), "done")["status"], "ok")
        self.repl.execute("interrupt-trace", INTERRUPT_TRACE_SETUP)
        self.repl.send({"type": "execute", "id": "blockedrepr", "code": "class BlockedRepr:\n    def __repr__(self):\n        _rt._send({'event':'test_trace','phase':'repr-start'})\n        time.sleep(600)\n        return 'must-not-succeed'\nBlockedRepr()"})
        events: list[dict] = []
        self._trace_until("repr-start", events)
        self.repl.send({"type": "interrupt", "id": "blockedrepr"})
        self._trace_until("interrupt-queued", events)
        events += self.repl.collect_for(0.35)
        self.assertFalse(any(event.get("event") in ("done", "result", "error") for event in events))
        self.assertTrue(all(not identity.exited() for identity in self.repl.identities))
        print(json.dumps({"nonresponsiveTrace": events, "action": "explicit isolated kernel kill"}))
        self.repl.close()
        self.repl = ScopedRepl(Path(self.directory.name))
        self.addCleanup(self.repl.close)
        self.repl.send({"type": "restore", "id": "restore", "path": str(state)})
        self.assertEqual(one(self.repl.until_done("restore"), "done")["status"], "ok")
        restored = self.repl.execute("after-restart", "import asyncio\nfrom rlm import bash\nsaved, (await bash('printf restored')).output")
        self.assertEqual(one(restored, "result")["text"], "({'proof': 31}, 'restored')")

    def test_bash_shutdown_checks_exact_native_process_identities(self) -> None:
        self.repl.execute("job-trace", JOB_TRACE_SETUP)
        result = self.repl.execute("echo", "from rlm import bash\n(await bash('echo repl-bash')).output.strip()")
        self.assertEqual(one(result, "result")["text"], "'repl-bash'")
        events = self.repl.execute("live-child", "import asyncio\nhandle = bash('sleep 600')\nawait asyncio.sleep(0.1)\n_job_pids(handle._job)")
        members = json.loads(one(events, "result")["text"])
        self.assertTrue(members)
        identities = [ProcessIdentity(pid) for pid in members]
        try:
            self.assertEqual(self.repl.shutdown(), 0)
            for identity in identities + self.repl.identities:
                self.assertTrue(identity.exited(5), f"owned PID {identity.pid} survived shutdown")
                print(json.dumps({"shutdownPid": identity.pid, "created": identity.created, "exited": True}))
        finally:
            for identity in identities:
                identity.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
