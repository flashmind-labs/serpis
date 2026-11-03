# What the disk image holds and how its window looks, for dmgbuild (scripts/make-dmg.sh): the app
# on the left, Applications on the right, so dragging it across reads left to right.
format = "UDZO"
files = [(defines["app"], "Serpis.app")]
symlinks = {"Applications": "/Applications"}
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
window_rect = ((200, 200), (600, 400))
default_view = "icon-view"
icon_size = 128
icon_locations = {"Serpis.app": (150, 180), "Applications": (450, 180)}
