"""Hands the add-on's settings to the atmos67 systemd service, and Kodi's
volume to the running sender.

The service reads its settings from atmos67.conf in the add-on's profile
directory, in the format of a systemd EnvironmentFile, and is restarted when
they change. Volume changes go straight to the sender's control socket, since
Kodi does not scale passthrough audio itself.
"""

import json
import os
import subprocess

import xbmc
import xbmcaddon
import xbmcgui
import xbmcvfs

import common

ADDON_ID = "service.atmos67"
UNITS = ["atmos67-ptp4l.service", "atmos67-phc2sys.service", "atmos67.service"]
SETTINGS = ["layout", "layout_file", "interface", "address", "ptp_domain", "buffer_ms", "mode", "subs", "slope",
            "max_volume_db"] + [f"xo_{group}" for group in common.GROUPS]


def addon():
    # A new object each time: an Addon object keeps the settings it was created with.
    return xbmcaddon.Addon(ADDON_ID)


def notify(message):
    xbmcgui.Dialog().notification(addon().getAddonInfo("name"), message, xbmcgui.NOTIFICATION_ERROR)


def profile():
    return xbmcvfs.translatePath(addon().getAddonInfo("profile"))


def settings():
    current = addon()
    values = {key: current.getSetting(key) for key in SETTINGS}
    layout_file = values["layout_file"]
    values["layout_file_path"] = xbmcvfs.translatePath(layout_file) if layout_file else ""
    return values


def kodi_volume():
    request = {"jsonrpc": "2.0", "id": 1, "method": "Application.GetProperties",
               "params": {"properties": ["volume", "muted"]}}
    result = json.loads(xbmc.executeJSONRPC(json.dumps(request))).get("result", {})
    return result.get("volume", 100), result.get("muted", False)


def volume_db(percent, muted):
    return common.volume_db(percent, muted, float(addon().getSetting("max_volume_db") or 0))


def write_config(mode=None):
    """Writes the service configuration; True when more than the volume changed."""
    path = os.path.join(profile(), "atmos67.conf")
    values = common.values(settings(), common.read_levels(profile()), profile(), volume_db(*kodi_volume()), mode)
    new = common.config_text(values)
    try:
        with open(path, encoding="utf-8") as file:
            old = file.read()
    except FileNotFoundError:
        old = ""
    keep = lambda text: [line for line in text.splitlines() if not line.startswith("VOLUME_DB=")]
    if keep(old) == keep(new):
        return False
    os.makedirs(profile(), exist_ok=True)
    with open(path, "w", encoding="utf-8") as file:
        file.write(new)
    return True


def restart():
    if not os.path.exists("/usr/bin/atmos67"):
        notify(addon().getLocalizedString(32040))
        return False
    result = subprocess.run(["sudo", "-n", "systemctl", "restart"] + UNITS, capture_output=True, text=True)
    if result.returncode != 0:
        xbmc.log(f"{ADDON_ID}: {result.stderr.strip()}", xbmc.LOGERROR)
        notify(addon().getLocalizedString(32041))
        return False
    return True


def apply(mode=None):
    if write_config(mode):
        restart()


def send_volume(percent, muted):
    try:
        if muted:
            common.command("mute 1")
        else:
            common.command(f"volume {volume_db(percent, False)}")
            common.command("mute 0")
    except OSError as error:
        # The service is starting or stopped; it starts at the volume written to its configuration.
        xbmc.log(f"{ADDON_ID}: volume not sent: {error}", xbmc.LOGDEBUG)


class Monitor(xbmc.Monitor):
    def onSettingsChanged(self):
        apply()

    def onNotification(self, sender, method, data):
        if method == "Application.OnVolumeChanged":
            event = json.loads(data)
            send_volume(event.get("volume", 100), event.get("muted", False))


if __name__ == "__main__":
    monitor = Monitor()
    apply()
    monitor.waitForAbort()
