"""Main window for the SACD ISO -> DSF extractor.

Layout (top -> bottom):
  * a large drop zone (drag & drop or click to browse)
  * disc info + extraction options
  * a track list table and a live progress/log panel
"""

from __future__ import annotations

import html
import sys
from pathlib import Path

from PySide6.QtCore import (
    QByteArray,
    QEvent,
    QSettings,
    Qt,
    QTimer,
    QUrl,
    Signal,
    Slot,
)
from PySide6.QtGui import (
    QColor,
    QDesktopServices,
    QDragEnterEvent,
    QDragLeaveEvent,
    QDragMoveEvent,
    QDropEvent,
    QIcon,
    QMouseEvent,
    QPainter,
    QPixmap,
)
from PySide6.QtWidgets import (
    QApplication,
    QCheckBox,
    QComboBox,
    QFileDialog,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLayout,
    QMainWindow,
    QProgressBar,
    QPushButton,
    QSizeGrip,
    QTableWidget,
    QTableWidgetItem,
    QToolButton,
    QVBoxLayout,
    QWidget,
)

from worker import ExtractWorker, ListWorker, find_tool

ISO_SUFFIXES = {".iso"}

# Window sizing: the window auto-fits the track list but never grows past
# these limits (a long list scrolls instead of stretching the window).
_MAX_TABLE_H = 420
_MAX_WINDOW_H = 820


def move_to_trash(path: str) -> None:
    """Move a file to the Recycle Bin / system trash (no external deps).

    Windows uses the Shell API (Recycle Bin), macOS uses Finder via
    ``osascript``, Linux tries ``gio trash``. Raises ``OSError`` when the
    file could not be moved.
    """
    if sys.platform == "win32":
        import ctypes
        from ctypes import wintypes

        class SHFILEOPSTRUCTW(ctypes.Structure):
            _fields_ = [
                ("hwnd", wintypes.HWND),
                ("wFunc", ctypes.c_uint),
                ("pFrom", wintypes.LPCWSTR),
                ("pTo", wintypes.LPCWSTR),
                ("fFlags", ctypes.c_ushort),
                ("fAnyOperationsAborted", wintypes.BOOL),
                ("hNameMappings", ctypes.c_void_p),
                ("lpszProgressTitle", wintypes.LPCWSTR),
            ]

        FO_DELETE = 3
        FOF_ALLOWUNDO = 0x40        # send to the Recycle Bin
        FOF_NOCONFIRMATION = 0x10   # never ask the user
        FOF_SILENT = 0x4
        FOF_NOERRORUI = 0x400

        struct = SHFILEOPSTRUCTW()
        struct.wFunc = FO_DELETE
        # pFrom is a double-null-terminated path list ("path\0" + the
        # implicit terminator ctypes stores after the Python string).
        struct.pFrom = str(path) + "\0"
        struct.fFlags = (
            FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI
        )

        shell32 = ctypes.windll.shell32
        shell32.SHFileOperationW.argtypes = [ctypes.POINTER(SHFILEOPSTRUCTW)]
        shell32.SHFileOperationW.restype = ctypes.c_int
        result = shell32.SHFileOperationW(ctypes.byref(struct))
        if result != 0 or struct.fAnyOperationsAborted:
            raise OSError(f"Recycle Bin operation failed (code {result})")
        if Path(path).exists():
            raise OSError("file still exists after the trash operation")
        return

    # macOS / Linux fallbacks.
    import subprocess

    if sys.platform == "darwin":
        script = f'tell application "Finder" to delete POSIX file "{path}"'
        rc = subprocess.run(
            ["osascript", "-e", script], capture_output=True
        ).returncode
    else:
        rc = subprocess.run(
            ["gio", "trash", str(path)], capture_output=True
        ).returncode
    if rc != 0 or Path(path).exists():
        raise OSError(f"could not move {path} to the trash")


# Open-source icons, all Apache License 2.0 — free to embed and redistribute.
#   * disc:   Material Design Icons "disc"
#             https://github.com/Templarian/MaterialDesign
#   * folder: Google Material Icons "folder"
#             https://github.com/google/material-design-icons
# Note: the official SACD wordmark/logo is a Sony/Philips trademark, so a
# generic open-source disc icon is used instead of any SACD-branded art.
DISC_SVG = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">'
    '<path d="M12,14C10.89,14 10,13.1 10,12C10,10.89 10.89,10 12,10C13.11,10 14,10.89 14,12A2,2 0 0,1 '
    '12,14M12,4A8,8 0 0,0 4,12A8,8 0 0,0 12,20A8,8 0 0,0 20,12A8,8 0 0,0 12,4Z"/>'
    '</svg>'
)
FOLDER_SVG = (
    '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">'
    '<path d="M10 4H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2h-8l-2-2z"/>'
    '</svg>'
)


def svg_pixmap(svg: str, size: int) -> QPixmap | None:
    """Rasterise a small inline SVG string to a square pixmap.

    Returns ``None`` (the icon is simply skipped) when QtSvg is unavailable,
    so the app keeps working without it.
    """
    try:
        from PySide6.QtSvg import QSvgRenderer
    except ImportError:  # pragma: no cover - QtSvg ships with PySide6
        return None
    renderer = QSvgRenderer(QByteArray(svg.encode("utf-8")))
    if not renderer.isValid():
        return None
    pix = QPixmap(size, size)
    pix.fill(Qt.transparent)
    painter = QPainter(pix)
    renderer.render(painter)
    painter.end()
    return pix


class DropZone(QFrame):
    """A large, clickable drop target for SACD ISO files."""

    filesDropped = Signal(list)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setAcceptDrops(True)
        self.setObjectName("dropZone")
        # Exactly half of the former 170 px height; the label below is sized
        # to fit inside it.
        self.setFixedHeight(85)

        self._label = QLabel(
            "Drag & drop a SACD <b>.iso</b> file here<br>— or —<br>click to browse",
            self,
        )
        self._label.setAlignment(Qt.AlignCenter)
        self._label.setObjectName("dropLabel")
        lay = QVBoxLayout(self)
        lay.setContentsMargins(6, 4, 6, 4)
        lay.addStretch(1)
        lay.addWidget(self._label)
        lay.addStretch(1)
        self.setStyleSheet(
            "#dropZone{border:2px dashed #7a7a7a;border-radius:14px;"
            "background:transparent;}"
            '#dropZone[dragging="true"]{border:2px dashed #2e9b3a;'
            "background:rgba(46,155,58,0.08);}"
            "#dropLabel{font-size:14px;}"
        )

    def mousePressEvent(self, event):  # noqa: D102
        paths, _ = QFileDialog.getOpenFileNames(
            self, "Select SACD ISO file(s)", "", "SACD ISO (*.iso);;All files (*)"
        )
        if paths:
            self.filesDropped.emit(paths)

    def set_dragging(self, dragging: bool) -> None:
        """Toggle the hover highlight while a file is dragged over the zone."""
        self.setProperty("dragging", dragging)
        # Re-evaluate the [dragging="true"] rule so the highlight shows/hides.
        self.style().unpolish(self)
        self.style().polish(self)
        # Mirror the state onto the whole window so the entire frame turns
        # green while a file is hovered anywhere over the app.
        win = self.window()
        if hasattr(win, "_window_drag_highlight"):
            win._window_drag_highlight(dragging, source=self)

    def dragEnterEvent(self, event: QDragEnterEvent) -> None:  # noqa: D102
        if event.mimeData().hasUrls():
            self.set_dragging(True)
            event.acceptProposedAction()

    def dragMoveEvent(self, event: QDragMoveEvent) -> None:  # noqa: D102
        if event.mimeData().hasUrls():
            event.acceptProposedAction()

    def dragLeaveEvent(self, event: QDragLeaveEvent) -> None:  # noqa: D102
        self.set_dragging(False)

    def dropEvent(self, event: QDropEvent) -> None:  # noqa: D102
        self.set_dragging(False)
        paths = []
        for url in event.mimeData().urls():
            p = url.toLocalFile()
            if p:
                paths.append(p)
        if paths:
            self.filesDropped.emit(paths)


class TitleBar(QWidget):
    """A custom title bar that replaces the native window frame.

    Hosts the window title, the always-on-top pin toggle, and the minimise /
    close buttons. Dragging the bar moves the (frameless) window; double-click
    toggles maximise.
    """

    pinToggled = Signal(bool)
    minimised = Signal()
    closed = Signal()

    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setObjectName("titleBar")
        self.setFixedHeight(40)
        self.setCursor(Qt.ArrowCursor)
        self._drag_pos = None

        lay = QHBoxLayout(self)
        lay.setContentsMargins(12, 0, 6, 0)
        lay.setSpacing(6)

        self.title = QLabel("SACD → DSF Extractor")
        self.title.setObjectName("titleLabel")
        lay.addWidget(self.title)
        lay.addStretch(1)

        self.pin_btn = QToolButton()
        self.pin_btn.setObjectName("pinBtn")
        self.pin_btn.setText("📌")
        self.pin_btn.setCheckable(True)
        self.pin_btn.setToolTip("Keep this window always on top of other windows.")
        self.pin_btn.setCursor(Qt.PointingHandCursor)
        # Styling for #pinBtn (hover / active states) is defined at the app
        # level in main() so all frameless controls share one stylesheet.
        self.pin_btn.toggled.connect(self.apply_pin_style)
        self.pin_btn.toggled.connect(self.pinToggled)
        # Always-on-top is on by default; MainWindow restores the persisted
        # value after construction.
        self.pin_btn.setChecked(True)
        lay.addWidget(self.pin_btn, 0, Qt.AlignVCenter)

        self.min_btn = QToolButton()
        self.min_btn.setObjectName("winBtn")
        self.min_btn.setText("—")
        self.min_btn.setToolTip("Minimise")
        self.min_btn.setCursor(Qt.PointingHandCursor)
        self.min_btn.clicked.connect(self.minimised)
        lay.addWidget(self.min_btn, 0, Qt.AlignVCenter)

        self.close_btn = QToolButton()
        self.close_btn.setObjectName("winBtnClose")
        self.close_btn.setText("✕")
        self.close_btn.setToolTip("Close")
        self.close_btn.setCursor(Qt.PointingHandCursor)
        self.close_btn.clicked.connect(self.closed)
        lay.addWidget(self.close_btn, 0, Qt.AlignVCenter)

    def apply_pin_style(self, checked: bool) -> None:
        """Visually reflect the pin state (highlighted when active)."""
        self.pin_btn.setProperty("active", checked)
        self.pin_btn.style().unpolish(self.pin_btn)
        self.pin_btn.style().polish(self.pin_btn)

    def mousePressEvent(self, event: QMouseEvent) -> None:  # noqa: D102
        if event.button() == Qt.LeftButton:
            self._drag_pos = (
                event.globalPosition().toPoint() - self.window().pos()
            )
            event.accept()

    def mouseMoveEvent(self, event: QMouseEvent) -> None:  # noqa: D102
        if self._drag_pos is not None and event.buttons() & Qt.LeftButton:
            win = self.window()
            if win.isMaximized() or win.isFullScreen():
                return
            win.move(event.globalPosition().toPoint() - self._drag_pos)
            event.accept()

    def mouseReleaseEvent(self, event: QMouseEvent) -> None:  # noqa: D102
        self._drag_pos = None

    def mouseDoubleClickEvent(self, event: QMouseEvent) -> None:  # noqa: D102
        if event.button() == Qt.LeftButton:
            win = self.window()
            win.showNormal() if win.isMaximized() else win.showMaximized()


class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("SACD → DSF Extractor")
        # Application/taskbar icon: the open-source disc icon (the same art
        # is used for QApplication so dialogs get it too).
        _app_pix = svg_pixmap(DISC_SVG, 64)
        if _app_pix is not None:
            self.setWindowIcon(QIcon(_app_pix))
        # Frameless window with our own title bar. The TitleBar (title text,
        # always-on-top pin, minimise/close) is docked into the window's
        # menu-widget strip, i.e. the very top edge of the window.
        self.setWindowFlag(Qt.FramelessWindowHint, True)
        # Wide enough for the whole options row (checkboxes + layer + Extract)
        # to fit on one line; the real minimum is computed from the layout
        # after the widgets are built (see below).
        self.setMinimumWidth(360)
        self.resize(880, 640)

        self.title_bar = TitleBar(self)
        self.setMenuWidget(self.title_bar)
        self.title_bar.pinToggled.connect(self.on_pin_toggled)
        self.title_bar.minimised.connect(self.showMinimized)
        self.title_bar.closed.connect(self.close)

        self.tool = find_tool()
        self.current_iso: str | None = None
        self.disc_info: dict | None = None
        self.list_worker: ListWorker | None = None
        self.extract_worker: ExtractWorker | None = None
        self._tracks_done = 0
        self._tracks_total = 0
        # Maps (disc_index, layer, track_number) -> table row so live
        # per-track status lands next to the right file even when several
        # discs are queued back to back.
        self._row_map: dict[tuple[int, str, int], int] = {}
        # Multi-disc queue: discs pending in the current batch, batch size,
        # finished count, and whether extraction already started (once it
        # has, the rest of the queue drains automatically).
        self._iso_queue: list[str] = []
        self._disc_total = 0
        self._disc_done = 0
        self._iso_queue: list[str] = []
        self._disc_total = 0
        self._disc_done = 0
        self._batch_active = False
        self._draining = False
        self._disc_index = 0          # disc currently LOADING (metadata)
        self._disc_loaded = 0         # discs fully loaded (ok or failed)
        self._disc_infos: dict[int, dict] = {}
        self._disc_paths: dict[int, str] = {}
        self._extract_queue: list[int] = []
        self._extract_disc_idx = 0    # disc currently EXTRACTING
        self._extracting = False
        # Last error text already displayed in the list; used to skip the
        # duplicate when the process exit carries the same message.
        self._last_error: str | None = None
        # Per-disc table sections: disc_index -> {header, last} row indexes.
        # The header holds the disc name + clickable folder link; "last" is
        # the section's final row so info/error rows land inside it.
        self._section_rows: dict[int, dict[str, int]] = {}
        # Desired always-on-top state, driven by the pin. Kept separate from
        # Qt's own window-flag model so the native Z-order can be re-applied
        # without ever re-parenting (and hiding) the visible frameless window.
        self._want_topmost = True
        # Window sizing: _fit_height() auto-fits the track list until the
        # user resizes the window themselves (then their size wins).
        self._fitting = False
        self._user_sized = False
        self._size_tracking = False
        self.settings = QSettings("SACD-Extractor", "SACD-Extractor")
        self._extract_iso: str | None = None
        self._extract_outdir: Path | None = None

        # The whole window is a drop target so the user can drop an ISO
        # anywhere (not just on the drop zone box).
        self.setAcceptDrops(True)

        central = QWidget()
        central.setObjectName("centralWidget")
        central.setAcceptDrops(True)
        self.setCentralWidget(central)

        # Translucent green overlay covering the *whole* window while a file
        # is hovered over it (raised above every child, including the title
        # bar). It ignores the mouse so drops and clicks pass through; it is
        # shown/hidden by _window_drag_highlight().
        self._drop_overlay = QFrame(self)
        self._drop_overlay.setObjectName("dropOverlay")
        self._drop_overlay.setAttribute(Qt.WA_TransparentForMouseEvents, True)
        self._overlay_label = QLabel("Drop SACD ISO here", self._drop_overlay)
        self._overlay_label.setObjectName("dropOverlayLabel")
        self._overlay_label.setAlignment(Qt.AlignCenter)
        overlay_lay = QVBoxLayout(self._drop_overlay)
        overlay_lay.addWidget(self._overlay_label)
        self._drop_overlay.hide()

        root = QVBoxLayout(central)
        root.setContentsMargins(18, 18, 18, 18)
        root.setSpacing(12)

        # --- Drop zone -------------------------------------------------
        self.drop = DropZone()
        self.drop.filesDropped.connect(self.on_files_dropped)
        root.addWidget(self.drop)

        # --- Options ---------------------------------------------------
        # Autostart + delete lead the row (first options in the row),
        # then layer/gapless, then the Extract button.
        opt = QHBoxLayout()

        self.auto_extract = QCheckBox("Autostart extract")
        self.auto_extract.setChecked(True)
        self.auto_extract.setToolTip(
            "Automatically start extracting as soon as a file is dropped or opened."
        )
        opt.addWidget(self.auto_extract)

        self.delete_iso = QCheckBox("Delete original ISO after extraction")
        self.delete_iso.setChecked(False)
        self.delete_iso.setToolTip(
            "After a successful extraction, move the original ISO to the\n"
            "Recycle Bin without asking again. Only once DSF output exists."
        )
        opt.addWidget(self.delete_iso)

        opt.addWidget(QLabel("Layer:"))
        self.layer_box = QComboBox()
        self.layer_box.addItem("Stereo", "stereo")
        self.layer_box.addItem("Multi-channel", "mch")
        self.layer_box.addItem("Both", "both")
        opt.addWidget(self.layer_box)

        self.gapless = QCheckBox("Gapless (no zero-pad)")
        self.gapless.setChecked(True)
        opt.addWidget(self.gapless)

        opt.addStretch(1)
        self.extract_btn = QPushButton("Extract")
        self.extract_btn.setEnabled(False)
        self.extract_btn.clicked.connect(self.start_extract)
        opt.addWidget(self.extract_btn)
        # Never squeeze this row below its natural width: on a narrow window
        # the checkboxes would otherwise clip their labels. The constraint
        # raises the window's minimum width to fit the row instead.
        opt.setSizeConstraint(QLayout.SetMinimumSize)
        root.addLayout(opt)

        # --- Track list ------------------------------------------------
        # Columns: # | Title | Performer | Duration | Status. The Status
        # column shows per-track live progress right next to each file.
        self.table = QTableWidget(0, 5)
        self.table.setHorizontalHeaderLabels(
            ["#", "Title", "Performer", "Duration", "Status"]
        )
        self.table.verticalHeader().setVisible(False)
        self.table.horizontalHeader().setSectionResizeMode(1, QHeaderView.Stretch)
        self.table.horizontalHeader().setSectionResizeMode(2, QHeaderView.Stretch)
        self.table.setColumnWidth(3, 70)
        self.table.setColumnWidth(4, 90)
        self.table.setEditTriggers(QTableWidget.NoEditTriggers)
        self.table.setSelectionBehavior(QTableWidget.SelectRows)
        # Cap the table height so a long list scrolls instead of stretching
        # the window past the maximum; short lists shrink the window to fit.
        self.table.setVerticalScrollBarPolicy(Qt.ScrollBarAsNeeded)
        self.table.setMaximumHeight(_MAX_TABLE_H)
        root.addWidget(self.table, 1)

        # Corner grip so the frameless window stays user-resizable.
        grip_row = QHBoxLayout()
        grip_row.addStretch(1)
        self._grip = QSizeGrip(self)
        self._grip.setFixedSize(16, 16)
        grip_row.addWidget(self._grip)
        root.addLayout(grip_row)

        # --- Progress --------------------------------------------------
        self.progress = QProgressBar()
        self.progress.setRange(0, 100)
        self.progress.setValue(0)
        self.progress.setTextVisible(True)
        self.progress.setFixedHeight(28)
        root.addWidget(self.progress)

        # The tool path is no longer surfaced in a status bar; the status
        # bar itself is hidden so the window stays compact.
        self.statusBar().hide()

        # The options row must never be squeezed below its natural width:
        # raise the window's minimum width to whatever the content needs so
        # the checkbox labels stay readable at any allowed window size.
        self.setMinimumWidth(max(360, self.centralWidget().minimumSizeHint().width()))

        self._restore_settings()

    # ------------------------------------------------------------------
    # --- Track list as the single info surface ------------------------
    def _shift_from(self, row: int, delta: int = 1) -> None:
        """Keep row indexes valid after inserting rows at ``row``."""
        for key, r in list(self._row_map.items()):
            if r >= row:
                self._row_map[key] = r + delta
        for meta in self._section_rows.values():
            if meta["header"] >= row:
                meta["header"] += delta
            if meta["last"] >= row:
                meta["last"] += delta

    def _insert_row_after(self, after_row: int, text: str, color: str,
                          bold: bool = False, italic: bool = True) -> int:
        """Insert a full-width info/error row below ``after_row``."""
        row = min(after_row + 1, self.table.rowCount())
        self.table.insertRow(row)
        self._shift_from(row, 1)
        item = QTableWidgetItem(f"  {text}")
        font = item.font()
        font.setItalic(italic)
        font.setBold(bold)
        item.setFont(font)
        item.setForeground(QColor(color))
        self.table.setItem(row, 0, item)
        for c in range(1, 5):
            self.table.setItem(row, c, QTableWidgetItem(""))
        self.table.setSpan(row, 0, 1, 5)
        self._fit_height()
        return row

    def _append_info(self, text: str, color: str = "#3f3f46",
                     bold: bool = False) -> int:
        """Append a global info row at the very end of the list."""
        return self._insert_row_after(
            self.table.rowCount() - 1, text, color, bold=bold
        )

    def _section_info(self, text: str, color: str = "#3f3f46",
                      bold: bool = False, for_disc: int | None = None) -> int:
        """Append an info row inside the given (default: current) disc's
        section."""
        idx = self._disc_index if for_disc is None else for_disc
        meta = self._section_rows.get(idx)
        after = meta["last"] if meta is not None else self.table.rowCount() - 1
        row = self._insert_row_after(after, text, color, bold=bold)
        if meta is not None:
            meta["last"] = max(meta["last"], row)
        return row

    def _track_error(self, layer: str, num, message: str) -> None:
        """Show an error in red directly under the affected track."""
        layer_name = "multichannel" if layer == "mch" else layer
        # Errors belong to the disc under *extraction*, which can already
        # lag behind the disc currently being loaded into the list.
        idx = self._extract_disc_idx or self._disc_index
        try:
            row = self._row_map.get((idx, layer_name, int(num)))
        except (TypeError, ValueError):
            row = None
        if row is None:
            self._section_info(f"✗ {message}", "#c0392b", for_disc=idx)
            return
        inserted = self._insert_row_after(row, f"✗ {message}", "#c0392b")
        meta = self._section_rows.get(idx)
        if meta is not None and row <= meta["last"]:
            meta["last"] = max(meta["last"], inserted)

    def _disc_header_label(self, name_html: str, path: str) -> QLabel:
        """Disc header widget: disc name + clickable folder link."""
        folder = Path(path).parent
        href = QUrl.fromLocalFile(str(folder)).toString()
        label = QLabel()
        label.setObjectName("discHeader")
        label.setTextFormat(Qt.RichText)
        label.setWordWrap(True)
        # Album name on the first line, the folder link on its own line
        # underneath so long paths have the full width to fit in.
        label.setText(
            f'{name_html}<br>'
            f'<a href="{href}">📁 {html.escape(str(folder))}</a>'
        )
        label.setToolTip("Click the path to open the disc's folder")
        label.linkActivated.connect(self._open_path_folder)
        return label

    @Slot(str)
    def _open_path_folder(self, href: str) -> None:
        """Open the folder whose link was clicked in a disc header."""
        QDesktopServices.openUrl(QUrl(href))

    @Slot(list)
    def on_files_dropped(self, paths: list) -> None:
        """Queue one or more ISO files; discs are processed back to back."""
        files: list[str] = []
        invalid: list[str] = []
        for raw in paths:
            path = str(raw)
            if not Path(path).is_file():
                invalid.append(f"Not a file: {path}")
                continue
            if Path(path).suffix.lower() not in ISO_SUFFIXES:
                invalid.append(f"Ignoring non-ISO file: {path}")
                continue
            files.append(path)
        if not files:
            if self._batch_active:
                # Report the problem inside the running batch.
                for msg in invalid:
                    self._append_info(f"✗ {msg}", "#c0392b")
            else:
                # Idle: report invalid drops on an empty, fresh list.
                self._reset_batch_ui()
                self.extract_btn.setEnabled(False)
                for msg in invalid:
                    self._append_info(f"✗ {msg}", "#c0392b")
            return

        fresh = not self._batch_active
        if fresh:
            # A brand-new batch: clear the previous run's list.
            self._batch_active = True
            self._reset_batch_ui()
        else:
            self._append_info(f"Added to queue: {len(files)} file(s)")

        for msg in invalid:
            self._append_info(f"✗ {msg}", "#c0392b")

        self._iso_queue.extend(files)
        self._disc_total += len(files)
        if not self._list_busy():
            # No metadata load in flight: start/continue loading right away
            # so every new disc shows in the list at once. While a load is
            # running, its completion chains into the next one instead.
            self._advance_queue()

    def _reset_batch_ui(self) -> None:
        """Clear the track list and queue counters for a fresh batch."""
        self._draining = False
        self._extracting = False
        self._extract_queue = []
        self._last_error = None
        self._disc_done = 0
        self._disc_index = 0
        self._disc_loaded = 0
        self._disc_infos = {}
        self._disc_paths = {}
        self._extract_disc_idx = 0
        self._disc_total = 0
        self._iso_queue = []
        self._row_map = {}
        self._section_rows = {}
        self.table.setRowCount(0)
        self.progress.setValue(0)

    def _advance_queue(self) -> None:
        """Load metadata for the next queued disc (loads run one by one)."""
        if not self._batch_active or self._list_busy() or not self._iso_queue:
            return

        path = self._iso_queue.pop(0)
        self._disc_index = self._disc_loaded + 1
        self.current_iso = path
        self._disc_paths[self._disc_index] = path
        self.disc_info = None
        self.extract_btn.setEnabled(False)
        self.progress.setValue(0)

        # Disc section header: file name placeholder + folder link from the
        # start; refreshed with the album name once metadata arrives.
        name = f"<b>{html.escape(Path(path).name)}</b> …"
        if self._disc_total > 1:
            name = f"<b>({self._disc_index}/{self._disc_total})</b> {name}"
        base = self.table.rowCount()
        self.table.setRowCount(base + 1)
        self.table.setItem(base, 0, QTableWidgetItem(""))
        self.table.setSpan(base, 0, 1, 5)
        self.table.setCellWidget(base, 0, self._disc_header_label(name, path))
        # Two lines: album name, then the folder link underneath.
        self.table.setRowHeight(base, 46)
        self._section_rows[self._disc_index] = {"header": base, "last": base}
        self._fit_height()
        self.load_metadata(path)

    def _list_busy(self) -> bool:
        return bool(self.list_worker and self.list_worker.isRunning())

    def load_metadata(self, path: str) -> None:
        self.list_worker = ListWorker(self.tool, path)
        self.list_worker.finished_ok.connect(self.on_metadata)
        self.list_worker.failed.connect(self.on_worker_error)
        self.list_worker.start()

    @Slot(dict)
    def on_metadata(self, info: dict) -> None:
        self.disc_info = info
        album = info.get("album") or "(unknown album)"
        artist = info.get("artist") or ""
        year = info.get("year") or ""
        name = f"<b>{html.escape(str(album))}</b>"
        if artist:
            name += f" — {html.escape(str(artist))}"
        if year:
            name += f" ({html.escape(str(year))})"
        if self._disc_total > 1:
            # Several discs queued: show "(n/total)" in front of the
            # currently processed disc's name (single discs stay clean).
            name = f"<b>({self._disc_index}/{self._disc_total})</b> {name}"
        # Refresh this disc's header row with the real album name.
        meta = self._section_rows.get(self._disc_index)
        if meta is not None:
            self.table.setCellWidget(
                meta["header"], 0,
                self._disc_header_label(name, self._disc_paths[self._disc_index]),
            )
        self._disc_infos[self._disc_index] = info
        self._disc_loaded += 1
        self._extract_queue.append(self._disc_index)
        self.populate_tracks(info)
        self.extract_btn.setEnabled(True)

        # Autostart kicks off the first disc, and once extraction has ever
        # started the rest keeps chaining automatically. Loading always
        # moves on immediately so every disc shows in the list at once.
        if not self._extracting:
            if self.auto_extract.isChecked():
                self._draining = True
            if self._draining:
                self._advance_extraction()
        self._advance_queue()

    def populate_tracks(self, info: dict) -> None:
        """Add layer info + track rows inside this disc's section."""
        areas = info.get("areas", {})
        rows: list[tuple] = []
        row_keys: list[tuple[str, int]] = []
        layer_lines: list[str] = []
        for layer in ("stereo", "multichannel"):
            area = areas.get(layer)
            if not area:
                continue
            ch = area.get("channels", "?")
            tracks = area.get("tracks", [])
            layer_lines.append(
                f"— {layer} ({ch}ch, "
                f"{area.get('track_count', len(tracks))} tracks) —"
            )
            for t in tracks:
                num = int(t.get("number"))
                row_keys.append((layer, num))
                rows.append(
                    (str(num), t.get("title", ""),
                     t.get("performer") or "", t.get("duration", ""),
                     "", f"[{ch}ch]")
                )

        meta = self._section_rows.get(self._disc_index)
        at = (meta["last"] + 1) if meta is not None else self.table.rowCount()
        total_new = len(layer_lines) + len(rows)
        for i in range(total_new):
            self.table.insertRow(at + i)
        if total_new:
            self._shift_from(at, total_new)

        for i, line in enumerate(layer_lines):
            item = QTableWidgetItem(f"  {line}")
            f = item.font()
            f.setItalic(True)
            item.setFont(f)
            item.setForeground(QColor("#6b7280"))
            r = at + i
            self.table.setItem(r, 0, item)
            for c in range(1, 5):
                self.table.setItem(r, c, QTableWidgetItem(""))
            self.table.setSpan(r, 0, 1, 5)

        for i, (num, title, perf, dur, status, tag) in enumerate(rows):
            r = at + len(layer_lines) + i
            layer, track_no = row_keys[i]
            self._row_map[(self._disc_index, layer, track_no)] = r
            self.table.setItem(r, 0, QTableWidgetItem(num))
            self.table.setItem(r, 1, QTableWidgetItem(title))
            self.table.setItem(r, 2, QTableWidgetItem(perf))
            self.table.setItem(r, 3, QTableWidgetItem(dur))
            self.table.setItem(r, 4, QTableWidgetItem(status))
            self.table.item(r, 0).setToolTip(tag)

        if meta is not None and total_new:
            meta["last"] = at + total_new - 1
        self._fit_height()

    def _fit_height(self) -> None:
        """Grow/shrink the window so the track list fits without wasted
        empty space, capped at ``_MAX_WINDOW_H``. Past that cap the list
        scrolls (the table has its own max height), so the window never
        grows unbounded. The user can still resize freely via the grip."""
        if not self.isVisible():
            return
        if self._user_sized:
            # The user picked a size; never fight them over it. The list
            # scrolls inside the table instead.
            return
        rows = self.table.rowCount()
        row_h = self.table.rowHeight(0) if rows else 0
        if row_h <= 0:
            row_h = 24  # sensible default before the first real layout
        header_h = self.table.horizontalHeader().height() or 24
        table_h = header_h + rows * row_h + 8
        table_h = min(table_h, _MAX_TABLE_H)
        # Swap the table's current viewport height for the desired one while
        # keeping every other widget's size fixed.
        current = self.table.viewport().height()
        new_h = self.height() + (table_h - current)
        new_h = max(new_h, 460)            # never collapse below a usable size
        new_h = min(new_h, _MAX_WINDOW_H)  # cap; the list scrolls beyond this
        if abs(new_h - self.height()) > 2:
            self._fitting = True
            try:
                self.resize(self.width(), new_h)
            finally:
                self._fitting = False

    def _set_track_status(self, layer: str, num: int, status: str) -> None:
        """Update the Status cell of a single track row, if present.

        Colour-codes the cell: green when done (✓), amber while in progress,
        red on error — so progress reads at a glance next to each track.
        Events always belong to the disc under extraction, which may lag
        behind the disc currently being listed.
        """
        layer_name = "multichannel" if layer == "mch" else layer
        idx = self._extract_disc_idx or self._disc_index
        row = self._row_map.get((idx, layer_name, int(num)))
        if row is None:
            return
        item = QTableWidgetItem(status)
        if status.startswith("✓"):
            item.setForeground(QColor("#1e8a2e"))
        elif "error" in status.lower():
            item.setForeground(QColor("#c0392b"))
        else:
            item.setForeground(QColor("#b9770e"))
        self.table.setItem(row, 4, item)

    @Slot(str)
    def on_worker_error(self, message: str) -> None:
        # The failure lands in red inside the disc's section; no dialog.
        self._section_info(f"✗ {message}", "#c0392b",
                           for_disc=self._disc_index)
        self.extract_btn.setEnabled(self.disc_info is not None)
        # A disc that cannot be listed is skipped so the queue continues.
        if self._batch_active:
            self._disc_loaded += 1
            self._advance_queue()

    # ------------------------------------------------------------------
    def output_dir_for(self) -> Path:
        # Output goes straight into the source ISO's folder (no subfolder),
        # as requested.
        return Path(self.current_iso).parent

    def _finish_batch_if_done(self) -> None:
        """Close the batch once every disc is loaded AND extracted."""
        if not self._batch_active:
            return
        if self._iso_queue or self._extract_queue or self._extracting:
            return
        if self._extract_worker and self.extract_worker.isRunning():
            return
        if self._disc_done < self._disc_total:
            return
        self._batch_active = False
        self._draining = False
        if self._disc_total > 1:
            self._append_info(
                f"All {self._disc_total} discs processed. ✓",
                "#1e8a2e", bold=True,
            )
        self._finish_batch_if_done()

    def _advance_extraction(self) -> None:
        """Start extracting the next queued disc, back to back.

        Extraction is strictly serial: it only moves on once the running
        extract worker has finished. Loads run ahead independently so the
        full track list is visible while extraction walks the discs.
        """
        if not self._batch_active or self._extracting:
            return
        if self.extract_worker and self.extract_worker.isRunning():
            return
        while self._extract_queue:
            idx = self._extract_queue.pop(0)
            if (idx in self._disc_infos
                    and self._disc_paths.get(idx)
                    and Path(self._disc_paths[idx]).is_file()):
                self._extract_disc_idx = idx
                break
            self._disc_done += 1
        else:
            self._extract_disc_idx = 0
            self._finish_batch_if_done()
            return

        self._extracting = True
        self.extract_btn.setEnabled(False)
        self._run_extract(self._extract_disc_idx)

    def _run_extract(self, idx: int) -> None:
        """Launch the extract worker for one already-listed disc."""
        path = self._disc_paths[idx]
        self.current_iso = path
        self.disc_info = self._disc_infos[idx]
        self._last_error = None
        self.progress.setValue(0)

        outdir = Path(path).parent
        outdir.mkdir(parents=True, exist_ok=True)
        self._extract_iso = path
        self._extract_outdir = outdir
        layer = self.layer_box.currentData()
        nopad = self.gapless.isChecked()

        self._tracks_done = 0
        self._tracks_total = 0
        self._section_info(f"→ Extracting to: {outdir}", "#3f3f46",
                           for_disc=idx)

        self.extract_worker = ExtractWorker(
            self.tool, path, str(outdir), layer, nopad
        )
        self.extract_worker.event.connect(self.on_extract_event)
        self.extract_worker.finished_ok.connect(self.on_extract_done)
        self.extract_worker.failed.connect(self.on_extract_error)
        self.extract_worker.start()

    def start_extract(self) -> None:
        if not self.disc_info or not self._extract_queue:
            return
        if self.extract_worker and self.extract_worker.isRunning():
            return
        # Extraction has started: the rest of the listed discs will now be
        # processed automatically, one after the other.
        self._draining = True
        self._advance_extraction()

    @Slot(dict)
    def on_extract_event(self, evt: dict) -> None:
        kind = evt.get("event")
        if kind == "layer_start":
            # Layer + channel counts are already listed in the disc section;
            # only the frame total is needed for the progress bar.
            self._tracks_total = int(evt.get("total", 0) or 0)
        elif kind == "track_start":
            self._set_track_status(
                evt.get("layer", ""), evt.get("num", 0), "Extracting…"
            )
        elif kind == "track_done":
            mb = (evt.get("bytes", 0) or 0) / (1024 * 1024)
            self._set_track_status(evt.get("layer", ""), evt.get("num", 0),
                                   f"✓ {mb:.1f} MB")
            self._tracks_done += 1
            if self._tracks_total:
                pct = int(self._tracks_done * 100 / self._tracks_total)
                self.progress.setValue(min(100, pct))
        elif kind == "warning":
            # Non-fatal problem (e.g. a corrupt sector that was skipped).
            self._section_info(f"⚠ {evt.get('message', '')}", "#b9770e",
                               for_disc=self._extract_disc_idx or self._disc_index)
        elif kind == "info":
            self._section_info(evt.get("message", ""), "#3f3f46",
                               for_disc=self._extract_disc_idx or self._disc_index)
        elif kind == "error":
            # Errors show in red under the affected track (or the section).
            message = evt.get("message", "error")
            self._last_error = str(message)
            num = evt.get("num")
            if num:
                self._track_error(evt.get("layer", ""), num, message)
            else:
                self._section_info(f"✗ {message}", "#c0392b",
                                   for_disc=self._extract_disc_idx or self._disc_index)
        elif kind == "log":
            self._section_info(evt.get("message", ""), "#6b7280",
                               for_disc=self._extract_disc_idx or self._disc_index)

    @Slot()
    def on_extract_done(self) -> None:
        self._extracting = False
        self.progress.setValue(100)
        self.extract_btn.setEnabled(True)
        self._section_info("✓ Disc finished", "#1e8a2e", bold=True,
                           for_disc=self._extract_disc_idx)
        self._maybe_delete_source()
        # Continue with the next queued disc (closes the batch when empty).
        self._disc_done += 1
        self._extract_disc_idx = 0
        if self._draining:
            self._advance_extraction()
        else:
            self._finish_batch_if_done()

    @Slot(str)
    def on_extract_error(self, message: str) -> None:
        self._extracting = False
        # The tool already emitted this exact text as an error event;
        # don't show the same failure twice.
        if str(message) != (self._last_error or ""):
            self._section_info(f"✗ {message}", "#c0392b",
                               for_disc=self._extract_disc_idx)
        self._last_error = None
        self.extract_btn.setEnabled(True)
        # Skip the failed disc and continue with the next one in the queue.
        self._disc_done += 1
        self._extract_disc_idx = 0
        if self._draining:
            self._advance_extraction()
        else:
            self._finish_batch_if_done()

    # ------------------------------------------------------------------
    @staticmethod
    def _as_bool(value, default: bool) -> bool:
        """Coerce a QSettings value to bool, tolerating string backends."""
        if value is None:
            return default
        if isinstance(value, bool):
            return value
        if isinstance(value, (int, float)):
            return bool(value)
        if isinstance(value, str):
            return value.strip().lower() in ("1", "true", "yes", "on")
        return default

    @Slot(bool)
    def _set_always_on_top(self, on_top: bool) -> None:
        """Toggle the always-on-top state without destabilising the window.

        The pin must never re-parent the visible window: calling
        ``setWindowFlag`` on an *already visible* widget makes Qt hide it (it
        re-parents internally), and re-showing a *frameless* ``QMainWindow``
        that hosts our TitleBar via ``setMenuWidget`` crashes the Windows
        compositor. So at runtime we only record the desired state and drive
        the native Z-order directly with ``SetWindowPos`` (Windows), which
        applies immediately and never re-creates the window. Qt's own flag is
        set once, before the first ``show()``, and re-applied from
        ``showEvent``/``changeEvent`` so minimise/restore keeps it in sync.
        """
        self._want_topmost = on_top

        if sys.platform == "win32":
            self._apply_native_topmost()
            return

        # Non-Windows fallback: the Qt flag + re-show is safe there.
        self.setWindowFlag(Qt.WindowStaysOnTopHint, on_top)
        if self.isVisible():
            QTimer.singleShot(0, self.show)

    def _apply_rounded_corners(self) -> None:
        """Give the frameless window slightly rounded corners on Windows 11.

        Uses the DWM ``DWMWA_WINDOW_CORNER_PREFERENCE`` attribute. On older
        Windows versions (or non-Windows) the call simply has no effect; it
        is fully wrapped so it can never crash the app.
        """
        if sys.platform != "win32":
            return
        try:
            import ctypes

            DWMWA_WINDOW_CORNER_PREFERENCE = 33
            DWMWCP_ROUND = 2  # slightly rounded corners

            dwmapi = ctypes.windll.dwmapi
            dwmapi.DwmSetWindowAttribute.argtypes = [
                ctypes.c_void_p,
                ctypes.c_int,
                ctypes.c_void_p,
                ctypes.c_uint,
            ]
            dwmapi.DwmSetWindowAttribute.restype = ctypes.c_long
            pref = ctypes.c_int(DWMWCP_ROUND)
            dwmapi.DwmSetWindowAttribute(
                int(self.winId()),
                DWMWA_WINDOW_CORNER_PREFERENCE,
                ctypes.byref(pref),
                ctypes.sizeof(pref),
            )
        except Exception:  # noqa: BLE001 - cosmetic, never crash
            pass

    def _apply_thick_frame(self) -> None:
        """Make the frameless window resizable from its edges.

        Windows only runs its sizing loop (edge drag, corner drag, arrow
        keys, Aero Snap) for windows carrying the ``WS_THICKFRAME`` style.
        ``nativeEvent()`` already answers WM_NCHITTEST with HTLEFT/HTRIGHT/
        HTTOP/... but without that style the answers are ignored, so every
        edge was dead and only the corner grip worked. On a frameless
        window the style adds no visible frame, and Qt re-creates the
        native handle whenever a window flag changes, so this is re-asserted
        from showEvent/changeEvent.
        """
        if sys.platform != "win32":
            return
        try:
            import ctypes

            user32 = ctypes.windll.user32
            # 64-bit-safe prototypes (same reasoning as _apply_native_topmost).
            user32.GetWindowLongW.argtypes = [ctypes.c_void_p, ctypes.c_int]
            user32.GetWindowLongW.restype = ctypes.c_long
            user32.SetWindowLongW.argtypes = [
                ctypes.c_void_p,
                ctypes.c_int,
                ctypes.c_long,
            ]
            user32.SetWindowLongW.restype = ctypes.c_long

            GWL_STYLE = -16
            WS_THICKFRAME = 0x00040000

            hwnd = ctypes.c_void_p(int(self.winId()))
            style = user32.GetWindowLongW(hwnd, GWL_STYLE)
            if not style & WS_THICKFRAME:
                user32.SetWindowLongW(hwnd, GWL_STYLE, style | WS_THICKFRAME)
        except Exception:  # noqa: BLE001 - cosmetic, never crash
            pass

    def _apply_native_topmost(self) -> None:
        """Apply ``_want_topmost`` to the native window via the Win32 API.

        Passing ``HWND_NOTOPMOST`` to ``SetWindowPos`` alone does **not**
        clear the window's ``WS_EX_TOPMOST`` extended-style bit, so turning
        the pin off used to leave the window stuck on top (confirmed with a
        native ``GetWindowLong`` probe). We therefore flip the
        ``WS_EX_TOPMOST`` bit explicitly with ``SetWindowLongW`` and re-assert
        the Z-order with ``SWP_FRAMECHANGED`` so the change takes effect.
        """
        try:
            import ctypes

            user32 = ctypes.windll.user32
            # 64-bit-safe prototypes: HWND is a pointer-sized handle, so the
            # default 32-bit int arg/return types would truncate it.
            user32.GetWindowLongW.argtypes = [ctypes.c_void_p, ctypes.c_int]
            user32.GetWindowLongW.restype = ctypes.c_long
            user32.SetWindowLongW.argtypes = [
                ctypes.c_void_p,
                ctypes.c_int,
                ctypes.c_long,
            ]
            user32.SetWindowLongW.restype = ctypes.c_long
            user32.SetWindowPos.argtypes = [
                ctypes.c_void_p,
                ctypes.c_void_p,
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_uint,
            ]
            user32.SetWindowPos.restype = ctypes.c_bool

            hwnd = int(self.winId())
            GWL_EXSTYLE = -20
            WS_EX_TOPMOST = 0x00000008
            HWND_TOPMOST, HWND_NOTOPMOST = -1, -2
            SWP_NOMOVE = 0x0002
            SWP_NOSIZE = 0x0001
            SWP_NOACTIVATE = 0x0010
            SWP_FRAMECHANGED = 0x0020

            style = user32.GetWindowLongW(hwnd, GWL_EXSTYLE)
            style = style | WS_EX_TOPMOST if self._want_topmost else style & ~WS_EX_TOPMOST
            user32.SetWindowLongW(hwnd, GWL_EXSTYLE, style)
            user32.SetWindowPos(
                hwnd,
                HWND_TOPMOST if self._want_topmost else HWND_NOTOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        except Exception:  #: noqa: BLE001 - never let a cosmetic toggle crash
            pass

    def changeEvent(self, event: QEvent) -> None:
        """Re-assert the native topmost bit after minimise/restore, etc.

        Qt re-applies its *own* window-flag model (fixed before the first
        ``show()``) whenever the window state changes, which would clobber a
        runtime pin toggle. We correct the native Z-order so the desired pin
        state survives.
        """
        if (
            event.type() == QEvent.Type.WindowStateChange
            and sys.platform == "win32"
        ):
            QTimer.singleShot(0, self._apply_native_topmost)
            # Minimise/restore rebuilds the native handle in some Qt
            # versions, which would drop the sizing style again.
            QTimer.singleShot(0, self._apply_thick_frame)
        super().changeEvent(event)

    def on_pin_toggled(self, checked: bool) -> None:
        """Toggle the always-on-top window flag when the pin is switched."""
        self._set_always_on_top(checked)



    def _restore_settings(self) -> None:
        """Restore persisted UI preferences (saved when the app closes)."""
        s = self.settings
        self.gapless.setChecked(
            self._as_bool(s.value("gapless"), self.gapless.isChecked())
        )
        self.auto_extract.setChecked(
            self._as_bool(s.value("auto_extract"), self.auto_extract.isChecked())
        )
        self.delete_iso.setChecked(
            self._as_bool(s.value("delete_iso"), self.delete_iso.isChecked())
        )
        # Always-on-top pin lives in the TitleBar. Default stays whatever the
        # TitleBar set (True); a saved value overrides it.
        saved_top = self._as_bool(
            s.value("always_on_top"), self.title_bar.pin_btn.isChecked()
        )
        self.title_bar.pin_btn.setChecked(saved_top)
        # Track the desired state explicitly (setChecked may not re-fire the
        # toggled slot if the value is unchanged).
        self._want_topmost = saved_top
        # Ensure Qt's window flag matches the final pin state before the first
        # show(). Safe here because the window is not yet visible.
        self.setWindowFlag(Qt.WindowStaysOnTopHint, saved_top)
        geometry = s.value("geometry")
        if geometry:
            self.restoreGeometry(geometry)
            # A saved geometry can predate a wider content minimum, so it
            # must not restore a window too narrow for the options row.
            if self.width() < self.minimumWidth():
                self.resize(self.minimumWidth(), self.height())

    def showEvent(self, event: QEvent) -> None:  # noqa: D102
        # On first show, re-apply the native topmost bit to match the pin so
        # the runtime state and Qt's flag model stay consistent.
        if sys.platform == "win32":
            QTimer.singleShot(0, self._apply_native_topmost)
            QTimer.singleShot(0, self._apply_rounded_corners)
            QTimer.singleShot(0, self._apply_thick_frame)
        # Any resize after this point is the user's doing, not startup
        # geometry, so auto-fit must stop respecting it.
        QTimer.singleShot(0, self._arm_size_tracking)
        super().showEvent(event)

    def _save_settings(self) -> None:
        """Persist UI preferences so they are restored on the next launch."""
        s = self.settings
        s.setValue("gapless", self.gapless.isChecked())
        s.setValue("auto_extract", self.auto_extract.isChecked())
        s.setValue("delete_iso", self.delete_iso.isChecked())
        s.setValue("always_on_top", self.title_bar.pin_btn.isChecked())
        s.setValue("geometry", self.saveGeometry())
        s.sync()

    def _maybe_delete_source(self) -> None:
        """Move the source ISO to the Recycle Bin after a successful run.

        No confirmation is shown: ticking "Delete original ISO" already
        *is* the user's confirmation. The file goes to the Recycle Bin
        (never a hard delete) so it can still be recovered.
        """
        if not self.delete_iso.isChecked():
            return
        iso = self._extract_iso
        outdir = self._extract_outdir
        if not iso or not Path(iso).is_file():
            return
        if not outdir or not outdir.is_dir():
            return

        # Safety: never touch the source unless real DSF output exists.
        dsf_files = sorted(outdir.glob("*.dsf"))
        if not dsf_files:
            self._section_info(
                "No DSF files were produced; keeping the original ISO.",
                "#b9770e",
            )
            return

        try:
            move_to_trash(iso)
        except OSError as exc:
            self._section_info(
                f"✗ Could not move the ISO to the Recycle Bin: {exc} "
                "(file kept)",
                "#c0392b",
            )
            return

        self._section_info(f"→ Moved to Recycle Bin: {Path(iso).name}",
                           "#1e8a2e")
        if self.current_iso == iso:
            self.current_iso = None
            self.extract_btn.setEnabled(False)

    def closeEvent(self, event) -> None:  # noqa: D102
        if self.extract_worker and self.extract_worker.isRunning():
            self.extract_worker.cancel()
            self.extract_worker.wait(3000)
        self._save_settings()
        super().closeEvent(event)

    # --- Whole-window drag & drop -----------------------------------
    # The window accepts drops anywhere (title bar, margins, table, etc.),
    # not only on the DropZone box. QDragEnterEvent/QDropEvent here mirror
    # the DropZone logic so the behaviour is identical regardless of where
    # the user releases the file.
    def _window_drag_highlight(self, on: bool, source=None) -> None:
        """Show/hide the full-window drop overlay on drag hover.

        The DropZone calls this when a file hovers directly over it, and the
        MainWindow drag handlers call it when the file is anywhere else over
        the window, so a single translucent green overlay covers the whole
        frame (title bar included) regardless of where the file is hovered.
        """
        if getattr(self, "_drag_highlight", False) == on:
            return
        self._drag_highlight = on
        if source is not self.drop:
            self.drop.set_dragging(on)
        overlay = getattr(self, "_drop_overlay", None)
        if overlay is None:
            return
        if on:
            overlay.setGeometry(self.rect())
            overlay.raise_()
            overlay.show()
        else:
            overlay.hide()

    # --- Resizing the frameless window -------------------------------
    # A window without a native frame has no resize borders, so Qt cannot
    # size it: the WM_NCHITTEST answer (which edge/corner the cursor is
    # over) is worked out here and handed back to Windows, which then runs
    # its usual sizing loop. The corner grip in the bottom-right keeps
    # working as a second handle.
    def nativeEvent(self, eventType, message):  # noqa: D102
        if sys.platform != "win32":
            return super().nativeEvent(eventType, message)
        import ctypes
        from ctypes import wintypes

        class MSG(ctypes.Structure):
            _fields_ = [
                ("hwnd", wintypes.HWND),
                ("message", ctypes.c_uint),
                ("wParam", ctypes.c_size_t),
                ("lParam", ctypes.c_size_t),
                ("time", ctypes.c_uint32),
                ("pt", wintypes.POINT),
            ]

        try:
            msg = MSG.from_address(int(message))
        except (TypeError, ValueError):
            return super().nativeEvent(eventType, message)
        if msg.message != 0x0084:  # WM_NCHITTEST
            return super().nativeEvent(eventType, message)

        # Cursor position in screen coordinates (signed: monitors placed
        # left of / above the primary one report negative values).
        x = ctypes.c_short(msg.lParam & 0xFFFF).value
        y = ctypes.c_short((msg.lParam >> 16) & 0xFFFF).value

        rect = wintypes.RECT()
        ctypes.windll.user32.GetWindowRect(int(self.winId()), ctypes.byref(rect))

        # Thin but grabbable border; the root layout's 18 px margins keep
        # it clear of the actual controls.
        border = 6
        on_left = x < rect.left + border
        on_right = x >= rect.right - border
        on_top = y < rect.top + border
        on_bottom = y >= rect.bottom - border

        HTCLIENT, HTLEFT, HTRIGHT, HTTOP = 1, 10, 11, 12
        HTTOPLEFT, HTTOPRIGHT, HTBOTTOM = 13, 14, 15
        HTBOTTOMLEFT, HTBOTTOMRIGHT = 16, 17

        if on_top and on_left:
            return True, HTTOPLEFT
        if on_top and on_right:
            return True, HTTOPRIGHT
        if on_bottom and on_left:
            return True, HTBOTTOMLEFT
        if on_bottom and on_right:
            return True, HTBOTTOMRIGHT
        if on_left:
            return True, HTLEFT
        if on_right:
            return True, HTRIGHT
        if on_top:
            return True, HTTOP
        if on_bottom:
            return True, HTBOTTOM
        return True, HTCLIENT

    def _arm_size_tracking(self) -> None:
        """Start counting manual resizes (runs just after the first show)."""
        self._size_tracking = True

    def resizeEvent(self, event) -> None:  # noqa: D102
        # Keep the drag overlay glued to the window frame at all sizes.
        super().resizeEvent(event)
        # Any size change _fit_height() did not drive itself (edge drag,
        # corner grip, maximise) means the user took over: from then on the
        # window keeps the size the user chose.
        if getattr(self, "_size_tracking", False) and not getattr(
            self, "_fitting", False
        ):
            self._user_sized = True
        overlay = getattr(self, "_drop_overlay", None)
        if overlay is not None:
            overlay.setGeometry(self.rect())

    def dragEnterEvent(self, event: QDragEnterEvent) -> None:  # noqa: D102
        if event.mimeData().hasUrls():
            event.acceptProposedAction()
            self._window_drag_highlight(True)

    def dragLeaveEvent(self, event: QDragLeaveEvent) -> None:  # noqa: D102
        self._window_drag_highlight(False)

    def dropEvent(self, event: QDropEvent) -> None:  # noqa: D102
        self._window_drag_highlight(False)
        paths = []
        for url in event.mimeData().urls():
            p = url.toLocalFile()
            if p:
                paths.append(p)
        if paths:
            self.on_files_dropped(paths)


def main() -> int:
    app = QApplication(sys.argv)
    app.setStyle("Fusion")
    # Application icon (taskbar / Alt-Tab / dialogs): the open-source disc.
    _app_icon = svg_pixmap(DISC_SVG, 64)
    if _app_icon is not None:
        app.setWindowIcon(QIcon(_app_icon))
    # Global style for the frameless title bar and its controls.
    app.setStyleSheet(
        "#titleBar{background:#efefef;border-top-left-radius:10px;"
        "border-top-right-radius:10px;}"
        "#titleLabel{font-size:13px;color:#3a3a3a;}"
        "#discHeader{background:#edf6ed;padding:5px 8px;font-size:12px;"
        "color:#000000;}"
        "#discHeader a{color:#000000;text-decoration:none;}"
        "#discHeader a:hover{color:#000000;text-decoration:underline;}"
        "#dropOverlay{border:3px dashed #2e9b3a;border-radius:12px;"
        "background:rgba(46,155,58,0.16);}"
        "#dropOverlayLabel{font-size:16px;font-weight:bold;color:#1e7a30;}"
        "QProgressBar{border:1px solid #c8c8d0;border-radius:6px;"
        "text-align:center;background:#e9e9ef;color:#1a1a1a;font-size:13px;}"
        "QProgressBar::chunk{background:#2e9b3a;border-radius:4px;}"
        "#pinBtn{border:none;background:transparent;font-size:18px;"
        "padding:2px 8px;border-radius:6px;}"
        "#pinBtn:hover{background:#dcdce2;}"
        '#pinBtn[active="true"]{background:#c4c4cf;}'
        "#winBtn{border:none;background:transparent;font-size:16px;"
        "padding:2px 10px;border-radius:6px;color:#3a3a3a;}"
        "#winBtn:hover{background:#dcdce2;}"
        "#winBtnClose:hover{background:#e81123;color:#ffffff;}"
    )
    win = MainWindow()
    win.show()
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())

