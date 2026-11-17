"""Hands the add-on's settings to the atmos67 systemd service.

The service reads them from atmos67.conf in the add-on's profile directory, in
the format of a systemd EnvironmentFile, and is restarted when they change.
"""

import os
import subprocess

import xbmc
import xbmcaddon
import xbmcgui
import xbmcvfs

ADDON_ID = "service.atmos67"
UNITS = ["atmos67-ptp4l.service", "atmos67-phc2sys.service", "atmos67.service"]


def notify(message):
    addon = xbmcaddon.Addon(ADDON_ID)
    xbmcgui.Dialog().notification(addon.getAddonInfo("name"), message, xbmcgui.NOTIFICATION_ERROR)


def quote(value):
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def config():
    # A new object each time: an Addon object keeps the settings it was created with.
    addon = xbmcaddon.Addon(ADDON_ID)
    layout_file = addon.getSetting("layout_file")
    values = {
        "LAYOUT": addon.getSetting("layout"),
        "LAYOUT_FILE": xbmcvfs.translatePath(layout_file) if layout_file else "",
        "INTERFACE": addon.getSetting("interface"),
        "ADDRESS": addon.getSetting("address"),
        "PTP_DOMAIN": addon.getSetting("ptp_domain"),
        "BUFFER_MS": addon.getSetting("buffer_ms"),
    }
    return "".join(f"{key}={quote(value)}\n" for key, value in values.items())


def apply():
    addon = xbmcaddon.Addon(ADDON_ID)
    profile = xbmcvfs.translatePath(addon.getAddonInfo("profile"))
    path = os.path.join(profile, "atmos67.conf")
    new = config()
    try:
        with open(path, encoding="utf-8") as file:
            if file.read() == new:
                return
    except FileNotFoundError:
        pass
    os.makedirs(profile, exist_ok=True)
    with open(path, "w", encoding="utf-8") as file:
        file.write(new)
    if not os.path.exists("/usr/bin/atmos67"):
        notify(addon.getLocalizedString(32040))
        return
    result = subprocess.run(
        ["sudo", "-n", "systemctl", "restart"] + UNITS, capture_output=True, text=True
    )
    if result.returncode != 0:
        xbmc.log(f"{ADDON_ID}: {result.stderr.strip()}", xbmc.LOGERROR)
        notify(addon.getLocalizedString(32041))


class Monitor(xbmc.Monitor):
    def onSettingsChanged(self):
        apply()


if __name__ == "__main__":
    monitor = Monitor()
    apply()
    monitor.waitForAbort()
