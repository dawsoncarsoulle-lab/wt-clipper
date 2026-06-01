#!/usr/bin/env python3
"""
Mock War Thunder localhost:8111 synchronisé avec une vidéo ET lancement de wt-clipper.

Workflow corrigé :
  1) démarre un faux serveur War Thunder sur 127.0.0.1:8111 ;
  2) lance la vidéo dans mpv, PAUSÉE, avec une vraie fenêtre déjà visible ;
  3) lance wt-clipper ;
  4) tu sélectionnes la fenêtre mpv dans wt-clipper ;
  5) quand tu confirmes, le script dépause la vidéo ET démarre le chrono des events.

Exemple depuis la racine du projet wt_clipper :
  python3 mock_wt_sync_video_launcher.py \
    --video "/chemin/vers/video.mp4" \
    --player dawson16800 \
    --clipper-cmd "cargo run --release -- gui"

Si tu veux que le script tente de dépauser automatiquement quand le backend semble prêt :
  python3 mock_wt_sync_video_launcher.py --video video.mp4 --start-after backend-ready

Dépendance : mpv installé.
"""

from __future__ import annotations

from dataclasses import dataclass
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs
from pathlib import Path
import argparse
import json
import os
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any


DEFAULT_VIDEO_NAME = "F-4S Phantom Showdown： Radar Missile Kill and Base Bombing - War Thunder Air Realistic Battle [P6IH7hb6Q-s].mp4"
READY_MARKERS = (
    "Replay buffer active",
    "Auto-clip armed",
    "watching for new events only",
    "buffer active",
)


@dataclass(frozen=True)
class MockEvent:
    id: int
    at: float
    msg: str
    kind: str = "event"
    duplicate_in_hud: bool = True


def fmt_game_time(seconds: float) -> str:
    seconds = max(0, int(round(seconds)))
    return f"{seconds // 60}:{seconds % 60:02d}"


class RuntimeState:
    def __init__(self, player: str, vehicle: str, events: list[MockEvent], time_offset: float):
        self.player = player
        self.vehicle = vehicle
        self.events = events
        self.time_offset = time_offset
        self.video_started_at: float | None = None
        self.video_process: subprocess.Popen[str] | None = None
        self.clipper_process: subprocess.Popen[str] | None = None
        self.lock = threading.Lock()

    def start_clock(self, proc: subprocess.Popen[str] | None = None) -> None:
        with self.lock:
            self.video_started_at = time.monotonic()
            self.video_process = proc

    def elapsed(self) -> float:
        with self.lock:
            started = self.video_started_at
        if started is None:
            return 0.0
        return max(0.0, time.monotonic() - started - self.time_offset)

    def active_events(self) -> list[MockEvent]:
        now = self.elapsed()
        return [event for event in self.events if now >= event.at]

    def next_event(self) -> MockEvent | None:
        now = self.elapsed()
        future = [event for event in self.events if event.at > now]
        return min(future, key=lambda event: event.at) if future else None


STATE: RuntimeState | None = None


class MockWarThunderHandler(BaseHTTPRequestHandler):
    server_version = "MockWarThunder8111/2.0"

    def log_message(self, fmt: str, *args: Any) -> None:
        if self.path.startswith(("/gamechat", "/hudmsg", "/state", "/indicators")):
            return
        print(f"[mock-wt] {self.address_string()} - {fmt % args}")

    def send_json(self, data: Any, status: int = 200) -> None:
        body = json.dumps(data, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        global STATE
        if STATE is None:
            self.send_json({"error": "mock not initialized"}, 500)
            return

        parsed = urlparse(self.path)
        qs = parse_qs(parsed.query)
        elapsed = STATE.elapsed()

        if parsed.path == "/state":
            self.send_json({
                "valid": True,
                "game_time": round(elapsed, 2),
                "army": "air",
                "speed": 900 + int((elapsed * 3) % 180),
                "altitude": 350 + int((elapsed * 2) % 120),
                "fuel": max(0, 30 - int(elapsed // 20)),
            })
            return

        if parsed.path == "/indicators":
            self.send_json({
                "valid": True,
                "type": "f_4s",
                "speed": 900 + int((elapsed * 3) % 180),
                "altitude_hour": 350 + int((elapsed * 2) % 120),
                "throttle": 1,
                "gear": 0,
                "mach": 0.85,
                "weapon1": 0,
                "weapon2": 0,
            })
            return

        if parsed.path == "/gamechat":
            last_id = int(qs.get("lastId", ["0"])[0] or 0)
            active = STATE.active_events()
            messages = [
                {"id": event.id, "time": fmt_game_time(event.at), "msg": event.msg}
                for event in active
                if event.id > last_id
            ]
            new_last_id = max([last_id] + [event.id for event in active])
            self.send_json({"lastId": new_last_id, "messages": messages})
            return

        if parsed.path == "/hudmsg":
            last_evt = int(qs.get("lastEvt", ["0"])[0] or 0)
            last_dmg = int(qs.get("lastDmg", ["0"])[0] or 0)
            active = STATE.active_events()

            hud_events = []
            hud_damage = []
            for event in active:
                if not event.duplicate_in_hud:
                    continue
                item = {"id": event.id, "time": fmt_game_time(event.at), "msg": event.msg}
                if event.id > last_evt:
                    hud_events.append(item)
                if event.id > last_dmg and event.kind in {"kill", "severe", "critical", "base"}:
                    hud_damage.append(item)

            new_last_evt = max([last_evt] + [event.id for event in active])
            new_last_dmg = max([last_dmg] + [event.id for event in active])
            self.send_json({
                "lastEvt": new_last_evt,
                "lastDmg": new_last_dmg,
                "events": hud_events,
                "damage": hud_damage,
            })
            return

        if parsed.path == "/map_info.json":
            self.send_json({"valid": True, "grid_size": [128, 128], "map_generation": 1})
            return

        if parsed.path == "/map_obj.json":
            self.send_json([])
            return

        self.send_json({"error": "not found", "path": parsed.path}, 404)


def build_default_events(player: str, vehicle: str) -> list[MockEvent]:
    """Timecodes calibrés sur la vidéo F-4S fournie."""
    return [
        MockEvent(1, 116.8, f"{player} ({vehicle}) severely damaged rbYI7-Vladjeep (F-4S)", "severe"),
        MockEvent(2, 119.4, f"{player} ({vehicle}) destroyed rbYI7-Vladjeep (F-4S)", "kill"),
        MockEvent(3, 151.7, f"{player} ({vehicle}) destroyed a base", "base"),
        MockEvent(4, 158.2, f"{player} ({vehicle}) critically damaged HEVT-Tooae (F-4S)", "critical"),
        MockEvent(5, 199.0, f"{player} ({vehicle}) severely damaged HEVT-Tooae (F-4S)", "severe"),
    ]


def start_server(host: str, port: int) -> ThreadingHTTPServer:
    server = ThreadingHTTPServer((host, port), MockWarThunderHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def wait_for_socket(path: Path, timeout: float = 5.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.05)
    raise TimeoutError(f"Le socket mpv IPC n'est pas apparu: {path}")


def mpv_command(ipc_socket: Path, command: list[Any]) -> Any:
    payload = json.dumps({"command": command}).encode("utf-8") + b"\n"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.connect(str(ipc_socket))
        sock.sendall(payload)
        data = sock.recv(4096)
    if not data:
        return None
    try:
        return json.loads(data.decode("utf-8"))
    except json.JSONDecodeError:
        return data.decode("utf-8", errors="replace")


def launch_video_paused(
    video_path: Path,
    mpv_bin: str,
    ipc_socket: Path,
    title: str,
    geometry: str | None,
    fullscreen_immediately: bool,
) -> subprocess.Popen[str]:
    try:
        ipc_socket.unlink()
    except FileNotFoundError:
        pass

    cmd = [
        mpv_bin,
        "--force-window=yes",
        "--pause=yes",
        "--keep-open=yes",
        "--osd-level=1",
        "--start=0",
        f"--title={title}",
        f"--input-ipc-server={ipc_socket}",
    ]
    if geometry:
        cmd.append(f"--geometry={geometry}")
    if fullscreen_immediately:
        cmd.append("--fs")
    cmd.append(str(video_path))

    print("[mock-wt] lancement vidéo PAUSÉE:", " ".join(shlex.quote(part) for part in cmd))
    proc = subprocess.Popen(cmd, text=True)
    wait_for_socket(ipc_socket)
    print(f"[mock-wt] fenêtre mpv prête, titre: {title!r}")
    return proc


def unpause_video(ipc_socket: Path, fullscreen_on_start: bool) -> None:
    if fullscreen_on_start:
        try:
            mpv_command(ipc_socket, ["set_property", "fullscreen", True])
        except Exception as exc:
            print(f"[mock-wt] warning: impossible de passer mpv en fullscreen: {exc}")
    mpv_command(ipc_socket, ["set_property", "pause", False])


def launch_clipper(command: str, cwd: Path | None, ready_event: threading.Event) -> subprocess.Popen[str]:
    print("[mock-wt] lancement wt-clipper:", command)
    proc = subprocess.Popen(
        command,
        cwd=str(cwd) if cwd else None,
        shell=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )

    def pump_output() -> None:
        assert proc.stdout is not None
        for line in proc.stdout:
            print(line, end="")
            lower = line.lower()
            if any(marker.lower() in lower for marker in READY_MARKERS):
                ready_event.set()

    threading.Thread(target=pump_output, daemon=True).start()
    return proc


def resolve_video_path(raw: str | None) -> Path:
    candidates: list[Path] = []
    if raw:
        candidates.append(Path(raw).expanduser())
    candidates.extend([
        Path.cwd() / DEFAULT_VIDEO_NAME,
        Path("/mnt/data") / DEFAULT_VIDEO_NAME,
    ])
    for path in candidates:
        if path.exists():
            return path.resolve()
    raise FileNotFoundError("Vidéo introuvable. Passe le chemin avec --video /chemin/vers/video.mp4")


def main() -> int:
    parser = argparse.ArgumentParser(description="Mock WT 8111 + vidéo mpv pausée + lancement wt-clipper")
    parser.add_argument("--video", default=None, help="Chemin de la vidéo à lancer avec mpv")
    parser.add_argument("--player", default="dawson16800", help="Pseudo à mettre dans les faux events")
    parser.add_argument("--vehicle", default="F-4S Phantom II", help="Véhicule à mettre dans les faux events")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8111)
    parser.add_argument("--mpv", default="mpv", help="Binaire mpv")
    parser.add_argument("--clipper-cmd", default="cargo run --release -- gui", help="Commande pour lancer wt-clipper")
    parser.add_argument("--cwd", default=None, help="Dossier depuis lequel lancer wt-clipper. Par défaut: dossier courant")
    parser.add_argument("--no-clipper", action="store_true", help="Ne lance pas wt-clipper, seulement serveur + vidéo pausée")
    parser.add_argument("--title", default="WT Clipper Mock Video", help="Titre de la fenêtre mpv")
    parser.add_argument("--geometry", default="1280x720+80+80", help="Géométrie mpv pendant la sélection, ex: 1280x720+80+80")
    parser.add_argument("--fullscreen", action="store_true", help="Passe mpv en plein écran au moment où la vidéo démarre")
    parser.add_argument("--fullscreen-immediately", action="store_true", help="Lance mpv directement en plein écran, même pendant la pause")
    parser.add_argument(
        "--start-after",
        choices=["enter", "backend-ready"],
        default="enter",
        help="enter = tu appuies sur Entrée après sélection fenêtre. backend-ready = tentative auto via logs wt-clipper.",
    )
    parser.add_argument(
        "--offset",
        type=float,
        default=0.0,
        help="Décalage des events en secondes. Positif = events plus tard, négatif = events plus tôt.",
    )
    args = parser.parse_args()

    video_path = resolve_video_path(args.video)
    events = build_default_events(args.player, args.vehicle)

    global STATE
    STATE = RuntimeState(args.player, args.vehicle, events, args.offset)

    server = start_server(args.host, args.port)
    ipc_socket = Path(tempfile.gettempdir()) / f"wt-clipper-mock-mpv-{os.getpid()}.sock"
    ready_event = threading.Event()

    print(f"[mock-wt] serveur prêt: http://{args.host}:{args.port}")
    print(f"[mock-wt] vidéo: {video_path}")
    print("[mock-wt] events prévus, relatifs au moment où la vidéo sera dépausée:")
    for event in events:
        print(f"  +{event.at:6.1f}s  [{event.kind}] {event.msg}")

    video_proc = launch_video_paused(
        video_path=video_path,
        mpv_bin=args.mpv,
        ipc_socket=ipc_socket,
        title=args.title,
        geometry=args.geometry,
        fullscreen_immediately=args.fullscreen_immediately,
    )
    STATE.video_process = video_proc

    clipper_proc: subprocess.Popen[str] | None = None
    cwd = Path(args.cwd).expanduser().resolve() if args.cwd else None
    if not args.no_clipper:
        clipper_proc = launch_clipper(args.clipper_cmd, cwd, ready_event)
        STATE.clipper_process = clipper_proc

    started = False

    def start_playback_once(reason: str) -> None:
        nonlocal started
        if started:
            return
        started = True
        print(f"\n[mock-wt] démarrage vidéo + chrono events ({reason})")
        unpause_video(ipc_socket, fullscreen_on_start=args.fullscreen)
        STATE.start_clock(video_proc)

    def shutdown(*_: Any) -> None:
        print("\n[mock-wt] arrêt...")
        try:
            server.shutdown()
        except Exception:
            pass
        for proc in (video_proc, clipper_proc):
            if proc and proc.poll() is None:
                try:
                    proc.terminate()
                except Exception:
                    pass
        try:
            ipc_socket.unlink()
        except FileNotFoundError:
            pass
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)

    if args.start_after == "backend-ready" and not args.no_clipper:
        print("\n[mock-wt] attente automatique d'un log backend prêt...")
        print("[mock-wt] Si ça ne démarre pas après avoir sélectionné la fenêtre, appuie sur Entrée ici.")

        def enter_fallback() -> None:
            input()
            start_playback_once("confirmation manuelle")

        threading.Thread(target=enter_fallback, daemon=True).start()
        while not started:
            if ready_event.wait(timeout=0.2):
                # Petite marge pour laisser la capture vraiment se stabiliser après le log.
                time.sleep(1.0)
                start_playback_once("backend prêt détecté")
                break
            if video_proc.poll() is not None:
                print("[mock-wt] mpv s'est fermé avant le démarrage")
                break
    else:
        print("\nÉtapes maintenant :")
        print("  1. La fenêtre vidéo mpv existe déjà et est en pause.")
        print("  2. Dans wt-clipper, sélectionne cette fenêtre, titre: WT Clipper Mock Video.")
        print("  3. Quand la sélection est terminée et que le buffer est prêt, reviens ici.")
        input("\nAppuie sur Entrée ici pour lancer la vidéo et démarrer les faux events...")
        start_playback_once("confirmation manuelle")

    try:
        while True:
            if video_proc.poll() is not None:
                print("\n[mock-wt] mpv terminé")
                break
            if clipper_proc is not None and clipper_proc.poll() is not None:
                print("\n[mock-wt] wt-clipper terminé")
                break

            elapsed = STATE.elapsed()
            if not started:
                print("\r[mock-wt] vidéo en pause | chrono events pas encore démarré", end="", flush=True)
            else:
                next_event = STATE.next_event()
                if next_event:
                    wait = max(0.0, next_event.at - elapsed)
                    print(f"\r[mock-wt] t={elapsed:6.1f}s | prochain event dans {wait:5.1f}s", end="", flush=True)
                else:
                    print(f"\r[mock-wt] t={elapsed:6.1f}s | tous les events ont été émis", end="", flush=True)
            time.sleep(0.5)
    finally:
        server.shutdown()
        try:
            ipc_socket.unlink()
        except FileNotFoundError:
            pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
