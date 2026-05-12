#!/usr/bin/env python3
import signal
import sys
from pathlib import Path
from urllib.parse import unquote, urlparse

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
gi.require_version("GdkPixbuf", "2.0")

from gi.repository import Gdk, GdkPixbuf, Gtk  # noqa: E402


def parse_wallpaper_path(raw_value: str) -> Path | None:
    if not raw_value:
        return None
    parsed = urlparse(raw_value)
    if parsed.scheme == "file":
        return Path(unquote(parsed.path))
    path = Path(raw_value)
    return path if path.exists() else None


class WallpaperWindow(Gtk.Window):
    def __init__(self, x: int, y: int, width: int, height: int, wallpaper_uri: str):
        super().__init__(type=Gtk.WindowType.TOPLEVEL)
        self.set_title("DesplioVirtualWallpaper")
        self.set_decorated(False)
        self.set_resizable(False)
        self.set_accept_focus(False)
        self.set_skip_taskbar_hint(True)
        self.set_skip_pager_hint(True)
        self.set_keep_below(True)
        self.stick()
        self.set_type_hint(Gdk.WindowTypeHint.DESKTOP)
        self.move(x, y)
        self.set_default_size(width, height)
        self.resize(width, height)

        self.width = width
        self.height = height
        self.pixbuf = self._load_wallpaper(width, height, wallpaper_uri)

        area = Gtk.DrawingArea()
        area.connect("draw", self.on_draw)
        self.add(area)

    def _load_wallpaper(self, width: int, height: int, wallpaper_uri: str):
        wallpaper_path = parse_wallpaper_path(wallpaper_uri)
        if wallpaper_path is None or not wallpaper_path.exists():
            return None

        try:
            original = GdkPixbuf.Pixbuf.new_from_file(str(wallpaper_path))
        except Exception:
            return None

        scale = max(width / original.get_width(), height / original.get_height())
        scaled_width = max(1, int(original.get_width() * scale))
        scaled_height = max(1, int(original.get_height() * scale))
        scaled = original.scale_simple(
            scaled_width,
            scaled_height,
            GdkPixbuf.InterpType.BILINEAR,
        )
        if scaled is None:
            return None

        offset_x = max(0, (scaled_width - width) // 2)
        offset_y = max(0, (scaled_height - height) // 2)
        return scaled.new_subpixbuf(offset_x, offset_y, width, height)

    def on_draw(self, _widget, cr):
        if self.pixbuf is not None:
            Gdk.cairo_set_source_pixbuf(cr, self.pixbuf, 0, 0)
            cr.paint()
            return False

        gradient = Gdk.RGBA()
        gradient.parse("#0f1722")
        cr.set_source_rgb(gradient.red, gradient.green, gradient.blue)
        cr.paint()
        return False


def main() -> int:
    if len(sys.argv) != 6:
        print("usage: x11_virtual_wallpaper.py <x> <y> <width> <height> <wallpaper-uri>", file=sys.stderr)
        return 2

    x = int(sys.argv[1])
    y = int(sys.argv[2])
    width = int(sys.argv[3])
    height = int(sys.argv[4])
    wallpaper_uri = sys.argv[5]

    window = WallpaperWindow(x, y, width, height, wallpaper_uri)
    window.connect("delete-event", Gtk.main_quit)
    signal.signal(signal.SIGTERM, lambda *_args: Gtk.main_quit())
    signal.signal(signal.SIGINT, lambda *_args: Gtk.main_quit())
    window.show_all()
    Gtk.main()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
