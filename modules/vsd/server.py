#!/usr/bin/env python3
"""VSD Companion Server — HLS stream downloader.

Downloads HLS streams (.m3u8 + .ts segments) and remuxes them into a single
MP4 file using ffmpeg. Designed to be managed by the MULA app.
"""

import os
import re
import sys
import uuid
import shutil
import logging
import platform
import tempfile
import threading
import subprocess
import urllib.parse
import urllib.request
from pathlib import Path
from time import time

from flask import Flask, request, jsonify
from flask_cors import CORS
from dotenv import load_dotenv

# ── Configuration ────────────────────────────────────────────────────────────

# Load .env from the same directory as this script
_HERE = Path(__file__).parent
load_dotenv(_HERE / ".env")

DEFAULT_DOWNLOAD_DIR = Path.home() / "Downloads" / "VSD"
DOWNLOAD_DIR = Path(os.environ.get("VSD_DOWNLOAD_DIR", str(DEFAULT_DOWNLOAD_DIR)))
FFMPEG_PATH = os.environ.get("VSD_FFMPEG", shutil.which("ffmpeg") or "ffmpeg")
SERVER_HOST = os.environ.get("VSD_HOST", "127.0.0.1")
SERVER_PORT = int(os.environ.get("VSD_PORT", "8765"))
LOG_DIR = Path(os.environ.get("VSD_LOG_DIR", str(_HERE / "logs")))
LOG_FILE = LOG_DIR / "vsd.log"
AUTO_OPEN = os.environ.get("VSD_AUTO_OPEN", "0").strip().lower() in ("1", "true", "yes")
MAX_RETRIES = int(os.environ.get("VSD_MAX_RETRIES", "3"))
JOB_TTL_SECONDS = 3600  # Remove finished jobs after 1 hour

DOWNLOAD_DIR.mkdir(parents=True, exist_ok=True)
LOG_DIR.mkdir(parents=True, exist_ok=True)

# ── Logging ──────────────────────────────────────────────────────────────────

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%H:%M:%S",
    handlers=[
        logging.FileHandler(LOG_FILE, encoding="utf-8"),
        logging.StreamHandler(sys.stdout),
    ],
)
logger = logging.getLogger("vsd")

# ── Flask App ────────────────────────────────────────────────────────────────

app = Flask(__name__)
CORS(app, resources={r"/*": {"origins": "*"}})

# ── Job Tracking ─────────────────────────────────────────────────────────────

_jobs: dict = {}
_jobs_lock = threading.Lock()


def _set_job(job_id: str, **kwargs):
    with _jobs_lock:
        if job_id not in _jobs:
            _jobs[job_id] = {
                "status": "running",
                "current": 0,
                "total": 0,
                "stage": "starting",
                "output": None,
                "error": None,
                "started_at": time(),
                "finished_at": None,
            }
        _jobs[job_id].update(kwargs)


def _cleanup_jobs():
    """Remove finished jobs older than JOB_TTL_SECONDS."""
    now = time()
    with _jobs_lock:
        expired = [
            jid for jid, j in _jobs.items()
            if j.get("finished_at") and (now - j["finished_at"]) > JOB_TTL_SECONDS
        ]
        for jid in expired:
            del _jobs[jid]


# ── Helpers ──────────────────────────────────────────────────────────────────

_DEFAULT_UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36"
)

_EXCLUDED_HEADERS = frozenset({
    "host", "accept-encoding", "connection", "content-length",
    "upgrade-insecure-requests"
})


def _clean_headers(headers: dict | None) -> dict:
    if not headers:
        return {}
    return {k: v for k, v in headers.items() if k.lower() not in _EXCLUDED_HEADERS}


def _fetch(url: str, headers: dict | None = None, retries: int = MAX_RETRIES) -> bytes:
    """Fetch a URL with retry logic."""
    req_headers = {"User-Agent": _DEFAULT_UA, **_clean_headers(headers)}
    req = urllib.request.Request(url, headers=req_headers)

    last_err = None
    for attempt in range(1, retries + 1):
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return resp.read()
        except Exception as e:
            last_err = e
            if attempt < retries:
                logger.warning(f"Fetch attempt {attempt}/{retries} failed for {url[:80]}: {e}")
    raise last_err


def _resolve_url(base: str, url: str) -> str:
    if url.startswith(("http://", "https://")):
        return url
    return urllib.parse.urljoin(base, url)


def _extract_resolution(m3u8_url: str, segments: list[str]) -> str:
    text = m3u8_url + " " + " ".join(segments[:5])
    match = re.search(r"(\d{3,4}[Pp])", text)
    return match.group(1).upper() if match else ""


def _sanitize_filename(title: str, max_len: int = 80) -> str:
    if not title:
        return ""
    name = re.sub(r'[<>:"/\\|?*]', "_", title)
    name = re.sub(r"[\x00-\x1f]", "", name)
    name = re.sub(r"\s+", "_", name)
    name = name.strip(" ._")
    if len(name) > max_len:
        name = name[:max_len].rsplit("_", 1)[0]
    return name


def _parse_m3u8(base_url: str, content: bytes) -> list[str]:
    """Extract .ts segment URLs from an M3U8 playlist."""
    text = content.decode("utf-8", errors="ignore")
    segments = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.endswith(".ts") or ".ts?" in line:
            segments.append(_resolve_url(base_url, line))
    return segments


def _open_file(path: Path):
    """Open a file with the system's default application."""
    try:
        if platform.system() == "Windows":
            os.startfile(str(path))
        elif sys.platform == "darwin":
            subprocess.Popen(["open", str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        else:
            subprocess.Popen(["xdg-open", str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except Exception as e:
        logger.warning(f"Could not open file: {e}")


# ── Download Logic ───────────────────────────────────────────────────────────

def _download_segments(
    segments: list[str], tmpdir: Path, headers: dict | None, job_id: str
) -> list[Path]:
    """Download all .ts segments to a temp directory."""
    total = len(segments)
    _set_job(job_id, total=total, current=0, stage="downloading")
    seg_files = []

    for i, url in enumerate(segments):
        ext = Path(urllib.parse.urlparse(url).path).suffix or ".ts"
        seg_path = tmpdir / f"seg_{i:05d}{ext}"
        try:
            data = _fetch(url, headers)
            seg_path.write_bytes(data)
            seg_files.append(seg_path)
            _set_job(job_id, current=len(seg_files))
        except Exception as e:
            logger.error(f"Segment {i+1}/{total} failed: {e}")

        if (i + 1) % 20 == 0 or i == total - 1:
            logger.info(f"Progress: {len(seg_files)}/{total} segments downloaded")

    return seg_files


def _mux_with_ffmpeg(seg_files: list[Path], output_path: Path, metadata: dict | None = None):
    """Concatenate .ts segments into a single MP4 using ffmpeg."""
    concat_list = output_path.parent / f".{output_path.stem}_concat.txt"
    concat_list.write_text(
        "".join(f"file '{seg.as_posix()}'\n" for seg in seg_files),
        encoding="utf-8",
    )

    cmd = [
        FFMPEG_PATH, "-y",
        "-f", "concat", "-safe", "0",
        "-i", str(concat_list),
        "-c", "copy",
        "-bsf:a", "aac_adtstoasc",
    ]
    for key, value in (metadata or {}).items():
        if value:
            cmd.extend(["-metadata", f"{key}={value}"])
    cmd.append(str(output_path))

    result = subprocess.run(cmd, capture_output=True)
    if result.returncode != 0:
        stderr = result.stderr.decode(errors="replace")[-500:]
        raise RuntimeError(f"ffmpeg failed (code {result.returncode}): {stderr}")

    concat_list.unlink(missing_ok=True)
    logger.info(f"Output: {output_path.name} ({output_path.stat().st_size // 1024} KB)")


def _build_output_name(title: str, resolution: str, job_id: str) -> str:
    parts = []
    if title:
        parts.append(title)
    if resolution:
        parts.append(resolution)
    parts.append(job_id)
    return "_".join(parts) + ".mp4"


def _build_metadata(title: str, raw: dict) -> dict:
    """Map request metadata to ffmpeg metadata keys."""
    m = {}
    if title:
        m["title"] = title
    if raw.get("pageUrl"):
        m["comment"] = raw["pageUrl"]
    if raw.get("channel"):
        m["artist"] = raw["channel"]
    if raw.get("categories"):
        cats = raw["categories"]
        m["genre"] = ", ".join(cats) if isinstance(cats, list) else str(cats)
    if raw.get("tags"):
        tags = raw["tags"]
        m["keywords"] = ", ".join(tags) if isinstance(tags, list) else str(tags)
    if raw.get("actors"):
        actors = raw["actors"]
        m["album_artist"] = ", ".join(actors) if isinstance(actors, list) else str(actors)
    if raw.get("uploadDate"):
        m["date"] = raw["uploadDate"]
    return m


def _run_download(job_id: str, url: str, title: str, headers: dict | None, raw_metadata: dict):
    """Main download pipeline — runs in a background thread."""
    safe_title = _sanitize_filename(title)
    logger.info(f"Job {job_id}: starting download — {safe_title or url[:80]}")

    try:
        tmpdir = Path(tempfile.mkdtemp(prefix=f"vsd_{job_id}_"))

        # Fetch and parse playlist
        playlist = _fetch(url, headers)
        segments = _parse_m3u8(url, playlist)
        if not segments:
            raise ValueError("No .ts segments found in playlist")
        logger.info(f"Job {job_id}: found {len(segments)} segments")

        # Download segments
        seg_files = _download_segments(segments, tmpdir, headers, job_id)
        if not seg_files:
            raise ValueError("No segments could be downloaded")

        # Determine output filename
        resolution = _extract_resolution(url, segments)
        output_name = _build_output_name(safe_title, resolution, job_id)
        output_path = DOWNLOAD_DIR / output_name

        # Mux
        _set_job(job_id, stage="muxing")
        metadata = _build_metadata(title, raw_metadata)
        _mux_with_ffmpeg(seg_files, output_path, metadata)

        # Cleanup temp
        shutil.rmtree(tmpdir, ignore_errors=True)

        # Auto-open if configured
        if AUTO_OPEN:
            _open_file(output_path)

        logger.info(f"Job {job_id}: completed — {output_path.name}")
        _set_job(
            job_id, status="done", stage="done",
            output=str(output_path),
            current=len(seg_files), total=len(segments),
            finished_at=time(),
        )

    except Exception as e:
        logger.error(f"Job {job_id}: failed — {e}")
        _set_job(job_id, status="error", stage="error", error=str(e), finished_at=time())


# ── API Endpoints ────────────────────────────────────────────────────────────

@app.route("/health", methods=["GET"])
def health():
    return jsonify({
        "ok": True,
        "ffmpeg": FFMPEG_PATH,
        "download_dir": str(DOWNLOAD_DIR),
    })


@app.route("/download", methods=["POST"])
def download():
    data = request.get_json(force=True)
    url = data.get("url")
    if not url:
        return jsonify({"error": "Missing 'url' field"}), 400

    title = data.get("title", "")
    headers = data.get("headers")
    raw_metadata = {
        "title": title,
        "pageUrl": data.get("pageUrl", ""),
        **(data.get("metadata") or {}),
    }

    job_id = uuid.uuid4().hex[:8]
    _set_job(job_id, status="running", stage="starting")
    threading.Thread(
        target=_run_download,
        args=(job_id, url, title, headers, raw_metadata),
        daemon=True,
    ).start()

    # Opportunistic cleanup of old jobs
    _cleanup_jobs()

    return jsonify({"ok": True, "job_id": job_id})


@app.route("/progress/<job_id>", methods=["GET"])
def progress(job_id):
    with _jobs_lock:
        job = _jobs.get(job_id)
    if not job:
        return jsonify({"error": "Unknown job"}), 404
    return jsonify(job)


@app.route("/logs", methods=["GET"])
def get_logs():
    lines = request.args.get("lines", "200", type=str)
    try:
        n = max(1, min(2000, int(lines)))
    except ValueError:
        n = 200

    if not LOG_FILE.exists():
        return jsonify({"log": ""})

    with open(LOG_FILE, "r", encoding="utf-8", errors="replace") as f:
        all_lines = f.readlines()
    return jsonify({"log": "".join(all_lines[-n:])})


@app.route("/open", methods=["POST"])
def open_file_endpoint():
    data = request.get_json(force=True)
    file_path = data.get("path")
    if not file_path:
        return jsonify({"error": "Missing 'path' field"}), 400

    path = Path(file_path)
    if not path.exists():
        return jsonify({"error": "File not found"}), 404

    _open_file(path)
    return jsonify({"ok": True})


# ── Entry Point ──────────────────────────────────────────────────────────────

if __name__ == "__main__":
    # Suppress Flask/Werkzeug dev server banner ("Press CTRL+C to quit", etc.)
    import click
    def _no_echo(*args, **kwargs):
        pass
    click.echo = _no_echo
    click.secho = _no_echo

    logger.info(f"VSD Server starting on http://{SERVER_HOST}:{SERVER_PORT}")
    logger.info(f"Download dir: {DOWNLOAD_DIR}")
    logger.info(f"ffmpeg: {FFMPEG_PATH}")
    app.run(host=SERVER_HOST, port=SERVER_PORT, debug=False, threaded=True)
