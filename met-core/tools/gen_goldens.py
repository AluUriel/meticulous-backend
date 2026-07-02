#!/usr/bin/env python3
"""Generate golden parity fixtures for the met-protocol Rust crate.

Runs the *real* Python parsers (esp_serial/data.py) plus a faithful copy of
the line dispatch in machine.py over a corpus of UART lines, and records the
outcome of every line as JSON. The Rust golden test (crates/protocol/tests/
golden.rs) replays the same lines and must match.

Corpus sources:
  - the emulator fixtures (esp_serial/connection/emulated.*.json), converted
    to wire lines the same way emulation_data.py does (via to_args()),
  - a hand-curated list of edge cases.

Usage (from the repo root):
  python3 met-core/tools/gen_goldens.py

Regenerate whenever esp_serial/data.py changes. Cases where Python raises an
uncaught exception are recorded with "raises": true — Rust must return a
graceful None for those (see met-core/README.md).
"""

import dataclasses
import json
import sys
import types
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
GOLDEN_DIR = REPO_ROOT / "met-core" / "crates" / "protocol" / "testdata" / "goldens"
FIXTURE_DIR = REPO_ROOT / "esp_serial" / "connection"

# esp_serial.data only needs a logger from the repo's log module; stub it so
# this script runs without the backend's dependencies installed.
log_stub = types.ModuleType("log")


class _StubLogger:
    @staticmethod
    def getLogger(*_args, **_kwargs):
        import logging

        logger = logging.getLogger("gen_goldens")
        logger.addHandler(logging.NullHandler())
        return logger


log_stub.MeticulousLogger = _StubLogger
sys.modules["log"] = log_stub
sys.path.insert(0, str(REPO_ROOT))

from esp_serial.data import (  # noqa: E402
    ButtonEventData,
    ESPInfo,
    HeaterTimeoutInfo,
    MachineNotify,
    SensorData,
    ShotData,
)

BARE_BUTTON_TOKENS = ["CCW", "CW", "push", "pu_d", "elng", "ta_d", "ta_l", "strt"]


def value_of(obj):
    """dataclass -> JSON-safe dict, matching serde field names in Rust."""
    if obj is None:
        return None
    if isinstance(obj, ButtonEventData):
        return {
            "event": obj.event.name,
            "time_since_last_event": obj.time_since_last_event,
        }
    return dataclasses.asdict(obj)


def parse_log(args):
    """Copy of the Log branch of Machine._read_data (machine.py)."""
    level = args[0].lower()
    message = args[1]
    full_message = ",".join(args[1:])
    send_to_sentry = False
    items_filtered = None
    if len(args) > 2:
        items = {}
        for item in args[2:]:
            parts = item.split("=")
            if len(parts) < 2:
                continue
            items.setdefault(parts[0], parts[1])
        send_to_sentry = items.get("sentry", "false") == "true"
        items_filtered = {k: v for k, v in items.items() if k != "sentry"}
    send_to_sentry = send_to_sentry or level == "error"
    return {
        "level": level,
        "message": message,
        "full_message": full_message,
        "items_filtered": items_filtered,
        "send_to_sentry": send_to_sentry,
    }


def dispatch(raw_line):
    """Copy of the token match in Machine._read_data (machine.py).

    Returns (kind, value, sio). Raises exactly where machine.py would let an
    exception escape into the read loop.
    """
    tokens = raw_line.strip("\r\n").split(",")
    match tokens:
        case [token] if token in BARE_BUTTON_TOKENS:
            ev = ButtonEventData.from_args([token])
            return "button", value_of(ev), ev.to_sio() if ev else None
        case ["Event", *rest]:
            ev = ButtonEventData.from_args(rest)
            return "button", value_of(ev), ev.to_sio() if ev else None
        case ["Data", *rest]:
            data = ShotData.from_args(rest)
            return "data", value_of(data), data.to_sio() if data else None
        case ["Sensors", color_coded]:
            sensor = SensorData.from_color_coded_args(color_coded)
            return "sensors", value_of(sensor), sensor.to_sio_sensors() if sensor else None
        case ["Sensors", *rest]:
            sensor = SensorData.from_args(rest)
            return "sensors", value_of(sensor), sensor.to_sio_sensors() if sensor else None
        case ["ESPInfo", *rest]:
            info = ESPInfo.from_args(rest)
            return "esp_info", value_of(info), info.to_sio() if info else None
        case ["Notify", *rest]:
            notify = MachineNotify(rest[0], ",".join(rest[1:]).replace(";", "\n"))
            return "notify", value_of(notify), None
        case ["HeaterTimeoutInfo", *rest]:
            try:
                heater = HeaterTimeoutInfo.from_args(rest)
            except Exception:
                # machine.py wraps this branch in try/except and logs.
                return "heater_timeout", None, None
            return "heater_timeout", value_of(heater), heater.to_dict()
        case ["Log", *rest]:
            try:
                return "log", parse_log(rest), None
            except Exception:
                # machine.py wraps the Log branch in try/except and logs.
                return "log", None, None
        case _:
            return "unrecognized", None, None


def build_case(raw_line):
    case = {
        "input": raw_line,
        "is_boot_banner": (
            raw_line.startswith("rst:0x")
            and "boot:0x" in raw_line
            and " (SPI_FAST_FLASH_BOOT)" in raw_line
        ),
        "has_crash_marker": any(
            marker in raw_line.lower()
            for marker in ["backtrace", "guru meditation error", "register dump"]
        ),
    }
    try:
        kind, value, sio = dispatch(raw_line)
        case.update({"kind": kind, "value": value, "sio": sio, "raises": False})
    except Exception:
        # An uncaught exception would crash the Python read loop; Rust must
        # instead classify the family and return message=None.
        kind = classify_only(raw_line)
        case.update({"kind": kind, "value": None, "sio": None, "raises": True})
    return case


def classify_only(raw_line):
    tokens = raw_line.strip("\r\n").split(",")
    first = tokens[0] if tokens else ""
    if len(tokens) == 1 and first in BARE_BUTTON_TOKENS:
        return "button"
    return {
        "Event": "button",
        "Data": "data",
        "Sensors": "sensors",
        "ESPInfo": "esp_info",
        "Notify": "notify",
        "HeaterTimeoutInfo": "heater_timeout",
        "Log": "log",
    }.get(first, "unrecognized")


def fixture_lines():
    """Wire lines rebuilt from the emulator fixtures, like emulation_data.py."""
    lines = []
    for name in ["emulated.shot.json", "emulated.home.json", "emulated.purge.json"]:
        fixture = json.loads((FIXTURE_DIR / name).read_text())
        samples = fixture["data"]
        # Sample the shot sparsely: full parity value at a fraction of the size.
        step = max(1, len(samples) // 40)
        for sample in samples[::step]:
            sensor = sample["sensors"]
            data = sample["shot"]
            if sample["time"] <= 0:
                continue
            new_sensors = SensorData(
                **{k: sensor[k] for k in sensor if k in SensorData.__dataclass_fields__}
            )
            setpoints_type = data["setpoints"]["active"]
            main_setpoint = None
            if setpoints_type is not None:
                main_setpoint = data["setpoints"].get(setpoints_type, None)
            new_shot = ShotData(
                pressure=data["pressure"],
                flow=data["flow"],
                weight=data["weight"],
                gravimetric_flow=data["gravimetric_flow"],
                temperature=data.get("temperature", sensor["tube"]),
                profile=fixture["profile_name"],
                status=sample["status"],
                main_controller_kind=setpoints_type,
                main_setpoint=main_setpoint,
            )
            lines.append("Data," + ",".join(new_shot.to_args()))
            lines.append("Sensors," + ",".join(new_sensors.to_args()))
    return lines


EDGE_CASES = [
    # Bare button tokens (only these eight dispatch without a prefix).
    *BARE_BUTTON_TOKENS,
    "tare",
    "ta_sl",
    "cntx",
    # Event-prefixed buttons with timing arguments.
    "Event,CCW,120",
    "Event,CW,9999+++",
    "Event,push,notanumber",
    "Event,ta_sl,42",
    "Event,cntx,7",
    "Event,tare_pressed,1",
    "Event,encoder_button_released,05",
    "Event,encoder_clockwise,3",
    "Event,unknown,1",
    "Event,doesnotexist,1",
    "Event,cw,1",
    "Event",
    "Event,",
    # ShotData variants.
    "Data,1.5,0.8,36.2,S,92.1,brewing,My%20Profile,Pressure,9.0,Flow,2.0,true,1.2",
    "Data,1.5,0.8,36.2,U,92.1,brewing,Profile,none,0.0,none,0.0,false,0.4",
    "Data,1.5,0.8,36.2,S,92.1,idle,idle",
    "Data,0.0,0.0,-0.4,U,23.2,idle,Purge",
    "Data,0.0,0.0,-0.4,U,23.2,retracting,Home",
    "Data,nan,NaN,bogus,S,92.0",
    "Data,1e-5,2E3,+3.5,S,1_0.5",
    "Data,1,2,3,S,20",
    "Data,1,2,3,S",
    "Data,1,2,3",
    "Data,1.5,0.8,36.2,S,92.1,Profile%20with%2C%20comma,name%E2%82%AC",
    "Data,1.5,0.8,36.2,S,92.1,bad%zzescape,%fF",
    "Data,1.5,0.8,36.2,S,92.1,heating,,Pressure,bogus,Flow,2.0,true,1.2",
    "Data,1.5,0.8,36.2,S,92.1,heating,p,Pressure,9.0,Flow,bogus,true,1.2",
    "Data,1.5,0.8,36.2,S,92.1,closing valve,p,Power,9.0,Flow,2.0,maybe,xyz",
    "Data",
    # SensorData variants.
    "Sensors,90.1,91.2,92.0,93.0,94.0,95.0,85.0,40.0,50.0,10.5,100.0,5.0,2.5,1.0,3.0,9.0,0.1,0.2,0.3,0.4,true,35.0,18.5",
    "Sensors,90.1,91.2,92.0,93.0,94.0,95.0,85.0,40.0,50.0,10.5,100.0,5.0,2.5,1.0,3.0,9.0,0.1,0.2,0.3,0.4,TRUE,nan,notanumber",
    "Sensors,inf,-inf,92.0,93.0,94.0,95.0,85.0,40.0,50.0,10.5,100.0,5.0,2.5,1.0,3.0,9.0,0.1,0.2,0.3,0.4,false,1_0,+2.5e-3",
    "Sensors,90.1,91.2,bogus,93.0,94.0,95.0,85.0,40.0,50.0,10.5,100.0,5.0,2.5,1.0,3.0,9.0,0.1,0.2,0.3,0.4,true,35.0,18.5",
    "Sensors,90.1,91.2",
    "Sensors",
    "Sensors,\x1b[1;31m ext1\x1b[0m90.1\x1b[1;32m ext2\x1b[0m91.2\x1b[1;33m bar_up\x1b[0m92.0\x1b[1;34m bar_mu\x1b[0m93.0\x1b[1;35m bar_md\x1b[0m94.0\x1b[1;36m bar_d\x1b[0m95.0\x1b[1;31m tube\x1b[0m85.0\x1b[1;32m mt\x1b[0m40.0\x1b[1;33m lt\x1b[0m50.0\x1b[1;34m mp\x1b[0m10.5\x1b[1;35m ms\x1b[0m100.0\x1b[1;36m mw\x1b[0m5.0\x1b[1;31m mc\x1b[0m2.5\x1b[1;32m bc\x1b[0m1.0\x1b[1;33m bp\x1b[0m3.0\x1b[1;34m ps\x1b[0m9.0\x1b[1;35m a0\x1b[0m0.1\x1b[1;36m a1\x1b[0m0.2\x1b[1;31m a2\x1b[0m0.3\x1b[1;32m a3\x1b[0m0.4\x1b[1;33m ws\x1b[0mtrue\x1b[1;34m mth\x1b[0m35.0\x1b[1;35m wp\x1b[0m18.5",
    "Sensors,\x1b[1;39m notacolor\x1b[0m90.1",
    # ESPInfo variants (arg-count history: 3, 8, 9, 10).
    "ESPInfo,v1.2.3,4,12.5",
    "ESPInfo,v1.2.3,fan_on,12.5",
    "ESPInfo,v1.2.3,4,12.5,black,SN123,B42,2024-01-15,scale-v2",
    "ESPInfo,v1.2.3,4,12.5,black,SN123,B42,2024-01-15,scale-v2,37.5",
    "ESPInfo,v1.2.3,4,12.5,black,SN123,B42,2024-01-15,scale-v2,37.5,true",
    "ESPInfo,v1.2.3,4,12.5,black,SN123,B42,2024-01-15,scale-v2,37.5,TRUE,extra",
    "ESPInfo,v1.2.3,4,12.5,black,SN123,B42,2024-01-15,scale-v2,bogus",
    "ESPInfo,v1.2.3,4,bogus",
    "ESPInfo,v1.2.3",
    "ESPInfo",
    # Notify.
    "Notify,warning,Water tank empty",
    "Notify,warning,line one;line two;line three",
    "Notify,info,message,with,commas",
    "Notify,onlytype",
    "Notify",
    # HeaterTimeoutInfo.
    "HeaterTimeoutInfo,10.5,60.0,5.0,30.0",
    "HeaterTimeoutInfo,0,60.0,0,30.0",
    "HeaterTimeoutInfo,10.5,60.0,5.0",
    "HeaterTimeoutInfo,10.5,60.0,5.0,30.0,extra",
    "HeaterTimeoutInfo,bogus,60.0,5.0,30.0",
    "HeaterTimeoutInfo",
    # Log lines.
    "Log,ERROR,Heater fault detected",
    "Log,info,boot complete",
    "Log,warning,low voltage,voltage=11.2,sentry=true",
    "Log,info,details,key=value=extra,malformed,k2=v2",
    "Log,info,details,sentry=false,a=1,a=2",
    "Log,debug,x,=empty,novalue=",
    "Log,info",
    "Log",
    # Boot banner / crash markers / noise.
    "rst:0x1 (POWERON_RESET),boot:0x13 (SPI_FAST_FLASH_BOOT)",
    "rst:0x1 (POWERON_RESET),boot:0x13 (RTC_SW_SYS_RESET)",
    "Backtrace: 0x40081234:0x3ffb1234",
    "Guru Meditation Error: Core 1 panic'ed (LoadProhibited)",
    "REGISTER DUMP:",
    "ets Jul 29 2019 12:21:46",
    "random garbage line",
    "",
    ",",
    "Data\r\n",
    "  CCW  ",
]


def main():
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    corpus = {
        "fixtures": fixture_lines(),
        "edge_cases": EDGE_CASES,
    }
    total = 0
    for name, lines in corpus.items():
        cases = []
        for line in lines:
            case = build_case(line)
            try:
                json.dumps(case, allow_nan=False)
            except ValueError:
                # inf/nan floats are not representable in strict JSON --
                # a documented divergence, keep those out of the goldens.
                print(f"skipping non-JSON-representable case: {line!r}")
                continue
            cases.append(case)
        out = GOLDEN_DIR / f"{name}.json"
        out.write_text(json.dumps(cases, indent=1, allow_nan=False) + "\n")
        print(f"wrote {out.relative_to(REPO_ROOT)} ({len(cases)} cases)")
        total += len(cases)
    print(f"total: {total} cases")


if __name__ == "__main__":
    main()
