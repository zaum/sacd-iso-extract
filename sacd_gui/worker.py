"""Background worker that runs the `sacd_tool` binary and parses its
JSON-line event stream. Emits Qt signals so the UI stays responsive.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from PySide6.QtCore import QThread, Signal

# On Windows, launching a child process from a GUI app can briefly flash a
# console window. CREATE_NO_WINDOW suppresses that so the extraction tool
# runs silently in the background.
_CREATE_NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0


def _popen_kwargs() -> dict:
    """Extra kwargs to spawn a child without flashing a console window."""
    return {"creationflags": _CREATE_NO_WINDOW} if _CREATE_NO_WINDOW else {}


def find_tool() -> str:
    """Locate the sacd_tool binary.

    Search order:
      1. ``SACD_TOOL`` environment variable (explicit override)
      2. A bundled ``tool/`` directory next to the frozen exe / this file
      3. A repo-local build under ``../sacd_tool/target/release``
      4. Fall back to ``sacd_tool`` on PATH.
    """
    import os
    import shutil

    override = os.environ.get("SACD_TOOL")
    if override and Path(override).exists():
        return override

    exe_names = (
        ["sacd-tool.exe", "sacd_tool.exe"]
        if sys.platform.startswith("win")
        else ["sacd-tool", "sacd_tool"]
    )

    # Frozen (PyInstaller) build: binaries live in a sibling ``tool`` dir.
    base = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent))
    for exe_name in exe_names:
        candidates = [
            base / "tool" / exe_name,
            base / exe_name,
            base.parent / "tool" / exe_name,
            # Source checkout layout: sacd_gui/ next to sacd_tool/
            base.parent / "sacd_tool" / "target" / "release" / exe_name,
        ]
        for cand in candidates:
            if cand.exists():
                return str(cand)

    for exe_name in exe_names:
        found = shutil.which(exe_name)
        if found:
            return found
    return exe_names[-1]


class ListWorker(QThread):
    """Runs ``sacd_tool list`` and emits the parsed JSON metadata."""

    finished_ok = Signal(dict)
    failed = Signal(str)

    def __init__(self, tool: str, iso: str, parent=None):
        super().__init__(parent)
        self.tool = tool
        self.iso = iso

    def run(self) -> None:  # noqa: D102 (Qt override)
        try:
            proc = subprocess.run(
                [self.tool, "list", "--iso", self.iso],
                capture_output=True,
                text=True,
                # The tool writes UTF-8; decode it explicitly so the Windows
                # ANSI codepage (cp1252) never turns accents into mojibake.
                encoding="utf-8",
                errors="replace",
                check=False,
                **_popen_kwargs(),
            )
        except FileNotFoundError:
            self.failed.emit(
                "Could not launch sacd_tool. Build it with `cargo build "
                "--release` in the sacd_tool folder, or set the SACD_TOOL "
                "environment variable to its path."
            )
            return
        except Exception as exc:  # pragma: no cover - defensive
            self.failed.emit(str(exc))
            return

        if proc.returncode != 0:
            detail = (proc.stderr or proc.stdout or "").strip()
            self.failed.emit(detail or f"sacd_tool list failed (exit {proc.returncode})")
            return

        try:
            data = json.loads(proc.stdout)
        except json.JSONDecodeError:
            self.failed.emit(f"Unexpected output from sacd_tool:\n{proc.stdout[:500]}")
            return

        self.finished_ok.emit(data)


class ExtractWorker(QThread):
    """Runs ``sacd_tool extract`` and streams parsed events to the UI."""

    event = Signal(dict)          # one parsed JSON-line event
    finished_ok = Signal()
    failed = Signal(str)

    def __init__(self, tool: str, iso: str, outdir: str, layer: str, nopad: bool,
                 parent=None):
        super().__init__(parent)
        self.tool = tool
        self.iso = iso
        self.outdir = outdir
        self.layer = layer
        self.nopad = nopad
        self._proc: subprocess.Popen | None = None

    def run(self) -> None:  # noqa: D102 (Qt override)
        args = [
            self.tool, "extract",
            "--iso", self.iso,
            "--output", self.outdir,
            "--layer", self.layer,
        ]
        if self.nopad:
            args.append("--dsf-nopad")

        try:
            self._proc = subprocess.Popen(
                args,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                # Same as ListWorker: force UTF-8 over cp1252.
                encoding="utf-8",
                errors="replace",
                bufsize=1,
                **_popen_kwargs(),
            )
        except FileNotFoundError:
            self.failed.emit(
                "Could not launch sacd_tool. Build it with `cargo build "
                "--release` in the sacd_tool folder, or set the SACD_TOOL "
                "environment variable to its path."
            )
            return
        except Exception as exc:  # pragma: no cover - defensive
            self.failed.emit(str(exc))
            return

        assert self._proc.stdout is not None
        last_error: str | None = None
        for line in self._proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                evt = json.loads(line)
            except json.JSONDecodeError:
                # Non-JSON noise (rare) — surface as a log line.
                self.event.emit({"event": "log", "message": line})
                continue
            if isinstance(evt, dict) and evt.get("event") == "error":
                msg = evt.get("message")
                if msg:
                    last_error = str(msg)
            self.event.emit(evt)

        code = self._proc.wait()
        if code != 0:
            # Prefer the tool's own error text over a bare exit code so the
            # same failure is not reported twice ("exited with code 1").
            self.failed.emit(last_error or f"sacd_tool exited with code {code}")
        else:
            self.finished_ok.emit()

    def cancel(self) -> None:
        """Request termination of the running extraction."""
        if self._proc and self._proc.poll() is None:
            self._proc.terminate()
