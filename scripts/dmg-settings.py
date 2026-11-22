# dmgbuild settings for Kass's DMG. cargo xtask dmg passes the app
# and the repo root in with -D.
#
# The icon positions match app/assets/dmg/background.html, which draws
# the glow behind Kass and the wave between the two icons.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild provides defines)
root = defines["root"]  # noqa: F821
app_name = os.path.basename(app)

format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(root, "app/icons/icon.icns")

# Points, not pixels: the background TIFF holds a 2x image for Retina.
background = os.path.join(root, "app/assets/dmg/background.tiff")
# The height includes the title bar (32 pt on macOS 26), leaving 400 for
# the background's design.
window_rect = ((200, 120), (660, 432))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False

icon_size = 128
text_size = 12
icon_locations = {
    app_name: (180, 170),
    "Applications": (480, 170),
}
