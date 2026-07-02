"""Contract test for phase 3 of the strangler rewrite: the Python IPC client
(esp_serial/rust_daemon_client.py) against the *real* met-daemon binary in
emulator mode. This is the seam the strangler cuts — if this passes, the
Python backend and the Rust daemon agree on the wire.

Skipped when the binary is not built. Build it with:
    cd met-core && cargo build -p met-ipc
or point MET_DAEMON_BIN at a prebuilt binary.
"""

import os
import shutil
import subprocess
import tempfile
import threading
import time

import pytest

from esp_serial.rust_daemon_client import DaemonEventHandler, RustDaemonClient

BACKEND_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DAEMON_BIN = os.environ.get(
    "MET_DAEMON_BIN",
    os.path.join(BACKEND_ROOT, "met-core", "target", "debug", "met-daemon"),
)

pytestmark = pytest.mark.skipif(
    not os.path.exists(DAEMON_BIN),
    reason=f"met-daemon binary not built at {DAEMON_BIN}",
)


class RecordingHandler(DaemonEventHandler):
    """Records every dispatched event; lets tests wait on predicates."""

    def __init__(self):
        self.lock = threading.Lock()
        self.hello = None
        self.events = []

    async def on_hello(self, version, snapshot):
        with self.lock:
            self.hello = {"version": version, "snapshot": snapshot}

    async def on_unknown(self, event_type, data):
        # Every event lands here on purpose: the test asserts on the raw
        # stream instead of duplicating MachineBridge logic.
        with self.lock:
            self.events.append({"type": event_type, **data})

    def __getattr__(self, name):
        raise AttributeError(name)

    def wait_for(self, description, predicate, timeout=15.0, start=0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self.lock:
                for event in self.events[start:]:
                    if predicate(event):
                        return event
            time.sleep(0.02)
        with self.lock:
            seen = sorted({e["type"] for e in self.events})
        pytest.fail(f"timed out waiting for {description}; seen event types: {seen}")


@pytest.fixture
def daemon_and_client():
    # Unix socket paths are capped at ~104 chars on macOS; pytest's tmp_path
    # blows past that, so build a short-lived dir under /tmp instead.
    tmp_dir = tempfile.mkdtemp(prefix="met-ipc-", dir="/tmp")
    socket_path = os.path.join(tmp_dir, "d.sock")
    env = {
        **os.environ,
        "BACKEND": "EMULATOR",
        "METICULOUS_EMULATION_DIR": os.path.join(BACKEND_ROOT, "esp_serial", "connection"),
        "EMULATION_SPEED": "8000",
        "METICULOUS_IPC_SOCKET": socket_path,
        "ALIVE_CHECK_SECONDS": "120",
    }
    process = subprocess.Popen(
        [DAEMON_BIN],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    handler = RecordingHandler()
    client = RustDaemonClient(handler, socket_path=socket_path)
    client.start()
    try:
        assert client.connected.wait(15.0), "client never connected to met-daemon"
        yield handler, client
    finally:
        process.terminate()
        process.wait(timeout=5)
        shutil.rmtree(tmp_dir, ignore_errors=True)


def test_hello_and_idle_playback(daemon_and_client):
    handler, _client = daemon_and_client

    handler.wait_for("status event", lambda e: e["type"] == "status")
    handler.wait_for("sensors event", lambda e: e["type"] == "sensors")

    with handler.lock:
        hello = handler.hello
    assert hello is not None
    assert hello["version"] == 1
    assert "data_sensors" in hello["snapshot"]

    idle = handler.wait_for(
        "idle status",
        lambda e: e["type"] == "status" and e["data"]["status"] == "idle",
    )
    # The payload must construct the backend dataclass directly - this is
    # what MachineBridge does with it.
    from esp_serial.data import ShotData

    shot = ShotData(**idle["data"])
    assert shot.profile == "idle"
    assert shot.is_extracting is False


def test_actions_drive_the_emulator(daemon_and_client):
    handler, client = daemon_and_client
    handler.wait_for("first status", lambda e: e["type"] == "status")

    assert client.send_action("purge")
    handler.wait_for(
        "purge status",
        lambda e: e["type"] == "status" and e["data"]["status"] == "purge",
    )
    handler.wait_for(
        "profile ready flip",
        lambda e: e["type"] == "profile_ready_changed" and e["ready"] is True,
    )


def test_profile_upload_reports_reference_hash(daemon_and_client):
    handler, client = daemon_and_client
    handler.wait_for("first status", lambda e: e["type"] == "status")

    assert client.send_profile({"name": "parity"})
    sent = handler.wait_for("profile_sent", lambda e: e["type"] == "profile_sent")
    # Same CPython-verified reference as the Rust unit test.
    assert sent["hash"] == "4d48a8609e2778855d2de253d7fb3e5d"


def test_flashing_handshake_round_trip(daemon_and_client):
    handler, client = daemon_and_client
    handler.wait_for("first status", lambda e: e["type"] == "status")

    assert client.release_port(bootloader=True), "port_released never arrived"
    assert client.acquire_port(), "port_resumed never arrived"
    released = handler.wait_for("port_released", lambda e: e["type"] == "port_released")
    assert released["bootloader"] is True
    # Playback continues after the handshake.
    with handler.lock:
        before = len(handler.events)
    handler.wait_for("status after resume", lambda e: e["type"] == "status", start=before)


def test_raw_write_reaches_the_esp(daemon_and_client):
    handler, client = daemon_and_client
    handler.wait_for("first status", lambda e: e["type"] == "status")

    # action,home as raw bytes, the path Machine.write() uses for NVS etc.
    assert client.write_raw(b"action,home\x03")
    handler.wait_for(
        "home playback",
        lambda e: e["type"] == "status" and e["data"]["profile"] == "Home",
    )
