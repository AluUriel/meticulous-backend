"""Client for the Rust serial daemon (met-core/crates/ipc, phase 3 of the
strangler rewrite).

Speaks the v1 IPC contract: newline-delimited JSON over a Unix socket.
Commands are fire-and-forget; completion is observed through the event
stream (`profile_sent`, `port_released`, `port_resumed`).

Two layers:
- ``RustDaemonClient``: transport + dispatch. Testable standalone against
  the real daemon binary (see tests/test_rust_ipc_client.py).
- ``MachineBridge``: wires daemon events to the same reactions
  ``Machine._read_data`` performs today (state, sio events, sounds, shot
  manager, alarms). Imports the heavy modules lazily so this file stays
  importable in isolation.
"""

import asyncio
import binascii
import json
import os
import threading

try:
    from named_thread import NamedThread
except (ImportError, AttributeError):  # pyprctl is Linux-only
    import threading as _threading

    class NamedThread(_threading.Thread):
        def __init__(self, name, *args, **kwargs):
            super().__init__(*args, name=name, **kwargs)


from log import MeticulousLogger

logger = MeticulousLogger.getLogger(__name__)

PROTOCOL_VERSION = 1
DEFAULT_SOCKET_PATH = os.getenv("METICULOUS_IPC_SOCKET", "/tmp/met-daemon.sock")
RECONNECT_DELAY_S = 1.0


class DaemonEventHandler:
    """Base handler: one async method per daemon event type, all no-ops.

    Method names follow the wire event types: ``on_<type>``.
    """

    async def on_hello(self, version, snapshot):
        pass

    async def on_snapshot(self, snapshot):
        pass

    async def on_unknown(self, event_type, data):
        logger.warning(f"Unhandled daemon event type: {event_type}")


class RustDaemonClient:
    """Connects to met-daemon, dispatches events, sends commands.

    Runs its own thread + asyncio loop, the same shape as the serial reader
    it replaces. Reconnects forever: if the daemon restarts (systemd), the
    backend just rejoins the stream.
    """

    def __init__(self, handler: DaemonEventHandler, socket_path=None):
        self.handler = handler
        self.socket_path = socket_path or DEFAULT_SOCKET_PATH
        self.connected = threading.Event()
        self.port_released = threading.Event()
        self.port_resumed = threading.Event()
        self._writer = None
        self._loop = None
        self._thread = None

    def start(self):
        self._thread = NamedThread("RustDaemonIPC", target=self._run_loop, daemon=True)
        self._thread.start()

    def _run_loop(self):
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)
        self._loop = loop
        loop.run_until_complete(self._connect_forever())

    async def _connect_forever(self):
        while True:
            try:
                reader, writer = await asyncio.open_unix_connection(self.socket_path)
            except (OSError, ConnectionError) as e:
                logger.warning(f"met-daemon socket unavailable ({self.socket_path}): {e}")
                await asyncio.sleep(RECONNECT_DELAY_S)
                continue

            logger.info(f"Connected to met-daemon at {self.socket_path}")
            self._writer = writer
            self.connected.set()
            try:
                while True:
                    line = await reader.readline()
                    if not line:
                        break
                    await self._dispatch_line(line)
            except (OSError, ConnectionError) as e:
                logger.warning(f"met-daemon connection lost: {e}")
            finally:
                self.connected.clear()
                self._writer = None
                writer.close()
            logger.warning("Disconnected from met-daemon, reconnecting")
            await asyncio.sleep(RECONNECT_DELAY_S)

    async def _dispatch_line(self, line):
        try:
            frame = json.loads(line)
        except json.JSONDecodeError as e:
            logger.error(f"Bad frame from met-daemon: {e}")
            return

        kind = frame.get("kind")
        if kind == "hello":
            version = frame.get("v")
            if version != PROTOCOL_VERSION:
                logger.error(
                    f"met-daemon speaks IPC v{version}, this client expects "
                    f"v{PROTOCOL_VERSION} - not processing events"
                )
                return
            await self.handler.on_hello(version, frame.get("snapshot"))
        elif kind == "event":
            event = frame.get("event", {})
            event_type = event.pop("type", "")
            if event_type == "port_released":
                self.port_released.set()
            elif event_type == "port_resumed":
                self.port_resumed.set()
            callback = getattr(self.handler, f"on_{event_type}", None)
            if callback is None:
                await self.handler.on_unknown(event_type, event)
            else:
                await callback(**event)
        elif kind == "snapshot":
            await self.handler.on_snapshot(frame.get("snapshot"))
        else:
            logger.warning(f"Unknown frame kind from met-daemon: {kind}")

    # --- commands (thread-safe, fire-and-forget) ---

    def _send(self, obj):
        loop = self._loop
        writer = self._writer
        if loop is None or writer is None:
            logger.error(f"Not connected to met-daemon, dropping command: {obj}")
            return False

        payload = (json.dumps(obj) + "\n").encode("utf-8")

        def write():
            if self._writer is not None:
                self._writer.write(payload)

        loop.call_soon_threadsafe(write)
        return True

    def send_action(self, name: str):
        return self._send({"kind": "action", "name": name})

    def send_profile(self, profile: dict):
        return self._send({"kind": "send_profile", "profile": profile})

    def write_raw(self, content: bytes):
        hex_payload = binascii.hexlify(content).decode("ascii")
        return self._send({"kind": "write_raw", "hex": hex_payload})

    def reset(self, bootloader=False):
        return self._send({"kind": "reset", "bootloader": bootloader})

    def request_snapshot(self):
        return self._send({"kind": "get_snapshot"})

    # --- flashing handshake (blocking, called from the update path) ---

    def release_port(self, bootloader=True, timeout=10.0) -> bool:
        """Ask the daemon to free the serial device (optionally holding the
        ESP in its bootloader) and wait for confirmation."""
        self.port_released.clear()
        self._send({"kind": "release_port", "bootloader": bootloader})
        return self.port_released.wait(timeout)

    def acquire_port(self, timeout=10.0) -> bool:
        """Hand the serial device back to the daemon and wait until it is
        reading again (the daemon hard-resets the ESP itself)."""
        self.port_resumed.clear()
        self._send({"kind": "acquire_port"})
        return self.port_resumed.wait(timeout)


class MachineBridge(DaemonEventHandler):
    """Applies daemon events to the backend, mirroring the reactions in
    ``Machine._read_data``. The daemon reports; this bridge decides —
    sounds, shot manager, notifications, alarms and flashing stay here."""

    def __init__(self, sio):
        self._sio = sio
        self._previous_preheat_remaining = None

    async def on_hello(self, version, snapshot):
        from machine import Machine

        logger.info(f"met-daemon IPC v{version} connected")
        # Resync flags the daemon owns; matters on daemon restarts, where
        # this side would otherwise keep stale readiness.
        if snapshot is not None:
            Machine.profileReady = snapshot.get("profile_ready", False)
            Machine.oldProfileReady = Machine.profileReady

    async def on_status(self, data):
        from machine import Machine
        from esp_serial.data import MachineStatus, ShotData

        Machine.data_sensors = ShotData(**data)
        Machine.is_idle = Machine.data_sensors.status == MachineStatus.IDLE
        Machine.infoReady = True

    async def on_sensors(self, data):
        from machine import Machine
        from esp_serial.data import SensorData
        from shot_debug_manager import ShotDebugManager
        from shot_manager import ShotManager

        Machine.sensor_sensors = SensorData(**data)
        Machine.stopMotorIfHot(Machine.data_sensors, Machine.sensor_sensors)
        ShotDebugManager.handleSensorData(Machine.sensor_sensors)
        ShotDebugManager.handleShotData(Machine.data_sensors)
        if Machine.data_sensors.is_extracting:
            ShotManager.handleSensorData(Machine.sensor_sensors)
            ShotManager.handleShotData(Machine.data_sensors)

    async def on_info(self, data):
        from machine import Machine
        from esp_serial.data import ESPInfo
        from config import (
            CONFIG_SYSTEM,
            CONFIG_USER,
            DISALLOW_FIRMWARE_FLASHING,
            MACHINE_BATCH_NUMBER,
            MACHINE_BUILD_DATE,
            MACHINE_COLOR,
            MACHINE_SERIAL_NUMBER,
            PROFILE_AUTO_PURGE,
            PROFILE_PARTIAL_RETRACTION,
            MeticulousConfig,
        )
        from manufacturing import FORCE_MANUFACTURING_ENABLED_KEY
        from config import CONFIG_MANUFACTURING

        info = ESPInfo(**data)
        Machine.esp_info = info
        Machine.infoReady = True
        Machine.firmware_running = Machine._parseVersionString(info.firmwareV)

        Machine.setPartialRetraction(
            float(MeticulousConfig[CONFIG_USER][PROFILE_PARTIAL_RETRACTION])
        )
        Machine.setAutoPurgeAfterShot(bool(MeticulousConfig[CONFIG_USER][PROFILE_AUTO_PURGE]))

        if (
            info.serialNumber != ""
            and info.serialNumber != "NOT_ASSIGNED"
            and info.color != ""
            and info.color != "NOT_ASSIGNED"
            and info.batchNumber != ""
            and info.batchNumber != "NOT_ASSIGNED"
            and info.buildDate != ""
            and info.buildDate != "NOT_ASSIGNED"
        ):
            MeticulousConfig[CONFIG_SYSTEM][MACHINE_SERIAL_NUMBER] = info.serialNumber
            MeticulousConfig[CONFIG_SYSTEM][MACHINE_COLOR] = info.color
            MeticulousConfig[CONFIG_SYSTEM][MACHINE_BATCH_NUMBER] = info.batchNumber
            MeticulousConfig[CONFIG_SYSTEM][MACHINE_BUILD_DATE] = info.buildDate
            MeticulousConfig.save()

        serial_assigned = MeticulousConfig[CONFIG_SYSTEM][MACHINE_SERIAL_NUMBER] is not None
        if Machine.enable_manufacturing != serial_assigned:
            if not MeticulousConfig[CONFIG_MANUFACTURING][FORCE_MANUFACTURING_ENABLED_KEY]:
                Machine.toggle_manufacturing_mode(enabled=False)

        logger.info(
            f"ESPInfo running firmware version: {Machine.firmware_running} "
            f"on pinout version {info.espPinout}"
        )
        needs_update = Machine.firmware_available is not None and (
            Machine.firmware_available != Machine.firmware_running
        )
        if needs_update and not MeticulousConfig[CONFIG_USER][DISALLOW_FIRMWARE_FLASHING]:
            logger.info("Firmware is outdated, upgrading")
            Machine.startUpdate()

    async def on_button(self, data):
        from machine import Machine
        from esp_serial.data import ButtonEventData, ButtonEventEnum

        event = ButtonEventData(
            ButtonEventEnum.from_str(data["event"]), data["time_since_last_event"]
        )
        await self._sio.emit("button", event.to_sio())
        if event.event is ButtonEventEnum.ENCODER_DOUBLE:
            logger.info("DOUBLE ENCODER, Returning to idle")
            Machine.end_profile()

    async def on_notify(self, data):
        from machine import Machine
        from notifications import Notification, NotificationManager, NotificationResponse

        if data["notificationType"] == "acaia_msg":
            response_options = []
        else:
            response_options = [NotificationResponse.OK]
        if Machine._espNotification.acknowledged:
            Machine._espNotification = Notification(data["message"], response_options)
        else:
            Machine._espNotification.message = data["message"]
            Machine._espNotification.respone_options = response_options
        logger.info(f"New Notification from ESP: {Machine._espNotification.message}")
        NotificationManager.add_notification(Machine._espNotification)

    async def on_heater_timeout(self, data):
        from machine import Machine
        from esp_serial.data import HeaterTimeoutInfo

        info = HeaterTimeoutInfo(**data)
        Machine.heater_timeout_info = info
        await self._sio.emit("heater_status", info.preheat_remaining)
        if info.preheat_remaining == 0 and self._previous_preheat_remaining != 0:
            logger.info("Heater_status: off")
        self._previous_preheat_remaining = info.preheat_remaining

    async def on_esp_log(self, data):
        import sentry_sdk

        level = data["level"]
        full_message = data["full_message"]
        log = getattr(
            logger, level if level in ("debug", "info", "warning", "error") else "info"
        )
        log(f"ESP {level}: {full_message}")

        if data["send_to_sentry"]:
            from machine import ESPSentryClient

            with sentry_sdk.new_scope() as scope:
                if data["items_filtered"] is not None:
                    scope.set_context("esp-data", data["items_filtered"])
                scope.set_client(ESPSentryClient)
                scope.capture_message(message=data["message"], level=level)

    async def on_shot_started(self, **_kwargs):
        from shot_manager import ShotManager
        from sounds import SoundPlayer, Sounds

        ShotManager.start()
        SoundPlayer.play_event_sound(Sounds.BREWING_START)

    async def on_shot_ended(self, **_kwargs):
        from shot_manager import ShotManager
        from sounds import SoundPlayer, Sounds

        SoundPlayer.play_event_sound(Sounds.BREWING_END)
        ShotManager.stop()

    async def on_went_idle(self, **_kwargs):
        from sounds import SoundPlayer, Sounds

        SoundPlayer.play_event_sound(Sounds.IDLE)

    async def on_heating_started(self, **_kwargs):
        from sounds import SoundPlayer, Sounds

        SoundPlayer.play_event_sound(Sounds.HEATING_START)

    async def on_heating_ended(self, **_kwargs):
        from sounds import SoundPlayer, Sounds

        SoundPlayer.play_event_sound(Sounds.HEATING_END)

    async def on_profile_ready_changed(self, ready):
        from machine import Machine
        from shot_debug_manager import ShotDebugManager

        if ready and not Machine.oldProfileReady:
            ShotDebugManager.start()
        if not ready and Machine.oldProfileReady:
            ShotDebugManager.stop()
        Machine.oldProfileReady = ready
        Machine.profileReady = ready

    async def on_profile_required(self, **_kwargs):
        logger.warning("Daemon refused start: no profile loaded on the ESP")

    async def on_boot_banner(self, reset_count):
        logger.info(f"ESP boot banner seen (reset_count={reset_count})")

    async def on_firmware_update_requested(self, **_kwargs):
        from machine import Machine

        logger.warning("The ESP seems to be resetting, sending update now")
        Machine.startUpdate()

    async def on_esp_silent_after_boot(self, **_kwargs):
        from machine import Machine

        Machine.check_machine_alive()

    async def on_alarm_raised(self, alarm):
        import sentry_sdk
        from api.alarms import AlarmManager, AlarmType

        kind = alarm.get("alarm")
        if kind == "esp_restart":
            trace = alarm.get("trace", "")
            with sentry_sdk.new_scope() as scope:
                if trace:
                    scope.set_extra("Tracing Info", trace)
                sentry_sdk.capture_message("ESP has restarted unexpectedly", "critical")
            AlarmManager.set_alarm(
                AlarmType.ESP_RESTART, end_time=None, force=False, quiet=True
            )
        elif kind == "esp_disconnected":
            sentry_sdk.capture_message("ESP has stopped communicating", "error")
            AlarmManager.set_alarm(
                AlarmType.ESP_DISCONNECTED, end_time=None, force=True, quiet=True
            )

    async def on_alarms_cleared(self, **_kwargs):
        from machine import Machine
        from api.alarms import AlarmManager, AlarmType

        Machine.esp_restart_request = False
        Machine.reset_count = 0
        AlarmManager.clear_alarm(AlarmType.ESP_DISCONNECTED)
        AlarmManager.clear_alarm(AlarmType.ESP_RESTART)

    async def on_port_released(self, bootloader):
        logger.info(f"met-daemon released the serial port (bootloader={bootloader})")

    async def on_port_resumed(self, **_kwargs):
        logger.info("met-daemon reacquired the serial port")
