"""Remove the battery-camera idle disconnect from the installed Reolink client.

Re-run after every Reolink update (updates overwrite resources/app). Needs an elevated shell.
Usage: python patch_battery_timeout.py ["C:\\Program Files\\Reolink"]
"""
import glob
import os
import shutil
import sys

root = sys.argv[1] if len(sys.argv) > 1 else r"C:\Program Files\Reolink"
app = os.path.join(root, "resources", "app")

# (file glob, original, patched, why)
PATCHES = [
    ("220.js",
     "case 4:if(!e.isBattery&&!r)return[2];if((o=e.onlineAlertTime)",
     "case 4:return[2];if((o=e.onlineAlertTime)",
     "deviceManager.noInteractionCallback: no idle countdown / auto close"),
    ("220.js",
     "case 4:if(t||!this.channel.device.isBattery)return[2];",
     "case 4:return[2];",
     "channelLiveProxy.closeDeviceForSavingPower: keep device open after live closes"),
    ("js/7755-*.js",
     "t._isClosedByClientSide||t._isBattery||(",
     "t._isClosedByClientSide||(",
     "VMSDevice: auto-reconnect battery devices like any other"),
]

failed = False
for pattern, old, new, why in PATCHES:
    matches = glob.glob(os.path.join(app, pattern))
    if len(matches) != 1:
        print(f"SKIP  {pattern}: expected 1 file, found {len(matches)}")
        failed = True
        continue
    path = matches[0]
    with open(path, encoding="utf-8") as f:
        src = f.read()
    if src.count(new) and not src.count(old):
        print(f"OK    {why} (already patched)")
        continue
    if src.count(old) != 1:
        print(f"FAIL  {why}: anchor found {src.count(old)}x in {path} (Reolink version changed?)")
        failed = True
        continue
    if not os.path.exists(path + ".orig"):
        shutil.copy2(path, path + ".orig")
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(src.replace(old, new))
    print(f"DONE  {why}")

sys.exit(1 if failed else 0)
