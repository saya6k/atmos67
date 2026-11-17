"""Room calibration wizard, run from the add-on's settings.

Restarts the service to measure (its speakers and subwoofers, without
correction), runs `atmos67 calibrate` for each microphone position the user
places, and switches to the new correction when SECS 2R has designed it. On
cancel or failure the previous settings come back.
"""

import os
import shutil
import socket
import subprocess
import time

import xbmc
import xbmcgui
import xbmcvfs

import common
import service

ATMOS67 = "/usr/bin/atmos67"


def text(string_id, *args):
    value = service.addon().getLocalizedString(string_id)
    return value % args if args else value


def wait_for_sender(seconds=30):
    """Waits until the restarted sender accepts connections."""
    for _ in range(seconds * 2):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
            if probe.connect_ex(common.CONTROL) == 0:
                return True
        time.sleep(0.5)
    return False


def run(dialog, args, heading):
    """Runs one calibration step, showing its progress; False when cancelled or failed."""
    process = subprocess.Popen([ATMOS67, "calibrate"] + args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    dialog.update(0, heading)
    for line in process.stdout:
        event = common.progress(line)
        if event:
            dialog.update(0, heading + "\n" + " ".join(part for part in event if part))
        if dialog.iscanceled():
            process.kill()
            process.wait()
            return False
    error = process.stderr.read()
    if process.wait() != 0:
        xbmc.log(f"{service.ADDON_ID}: calibrate {args[0]}: {error}", xbmc.LOGERROR)
        xbmcgui.Dialog().ok(text(32100), text(32108) + "\n" + error.strip()[-400:])
        return False
    return True


def calibrate():
    addon = service.addon()
    profile = service.profile()
    settings = service.settings()
    positions = int(addon.getSetting("positions") or 1)
    microphone = addon.getSetting("microphone")
    if not xbmcgui.Dialog().yesno(text(32100), text(32101, positions)):
        return
    project = os.path.join(profile, "calibration")
    shutil.rmtree(project, ignore_errors=True)
    service.apply(mode="measure")
    if not wait_for_sender():
        xbmcgui.Dialog().ok(text(32100), text(32102))
        return
    layout = ["--layout-file", settings["layout_file_path"]] if settings["layout_file_path"] else ["--layout", settings["layout"]]
    common_args = ["--project", project, "--control", common.CONTROL, "--mic", microphone]
    start = ["start"] + layout + ["--subs", settings["subs"]] + common_args
    mic_cal = addon.getSetting("mic_cal")
    if mic_cal:
        start += ["--mic-cal", xbmcvfs.translatePath(mic_cal)]

    dialog = xbmcgui.DialogProgress()
    dialog.create(text(32100))
    try:
        if not run(dialog, start, text(32103)):
            return
        for position in range(1, positions + 1):
            dialog.update(0, text(32104, position, positions))
            if not xbmcgui.Dialog().yesno(text(32100), text(32104, position, positions)):
                return
            if not run(dialog, ["measure", "--position", str(position)] + common_args, text(32105, position)):
                return
        designs = os.path.join(profile, "designs")
        generation = os.path.join(designs, time.strftime("%Y%m%d-%H%M%S"))
        os.makedirs(designs, exist_ok=True)
        if not run(dialog, ["finish", "--out", generation] + common_args, text(32106)):
            return
        # The service reads the design through this link; switch it in one step.
        link = common.auto_design(profile)
        os.symlink(os.path.basename(generation), link + ".new")
        os.replace(link + ".new", link)
        addon.setSetting("mode", str(common.MODES.index("auto")))
    finally:
        dialog.close()
        # Back to the settings: the new correction, or the old one when cancelled.
        service.apply()
    xbmcgui.Dialog().ok(text(32100), text(32107))


if __name__ == "__main__":
    calibrate()
