#!/usr/bin/env python3
"""VSD Companion Server — HLS stream downloader.

Downloads HLS streams (.m3u8 + .ts segments) and remuxes them into a single
MP4 file using ffmpeg. Designed to be managed by the MULA app.
"""

import os
import re
import ssl
import sys
import hmac
import uuid
import shutil
import logging
import platform
import tempfile
import threading
import subprocess
import http.client
import urllib.error
import urllib.parse
import urllib.request
from collections import deque
from concurrent.futures import ThreadPoolExecutor, as_completed
from logging.handlers import RotatingFileHandler
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
# Segments downloaded in parallel per job (clamped to 1..16).
SEGMENT_WORKERS = max(1, min(16, int(os.environ.get("VSD_SEGMENT_WORKERS", "6"))))
FETCH_TIMEOUT = 30  # seconds, per HTTP request
MAX_REDIRECTS = 10
JOB_TTL_SECONDS = 3600  # Remove finished jobs after 1 hour

# Per-launch secret from MULA that authorizes configuration changes. Removed from
# the environment so ffmpeg and opened files never inherit it.
ADMIN_TOKEN = os.environ.pop("VSD_ADMIN_TOKEN", "").strip()
ADMIN_TOKEN_HEADER = "X-VSD-Admin-Token"

DOWNLOAD_DIR.mkdir(parents=True, exist_ok=True)
LOG_DIR.mkdir(parents=True, exist_ok=True)

# ── Logging ──────────────────────────────────────────────────────────────────

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%H:%M:%S",
    handlers=[
        RotatingFileHandler(LOG_FILE, maxBytes=5 * 1024 * 1024, backupCount=2, encoding="utf-8"),
        logging.StreamHandler(sys.stdout),
    ],
)
logger = logging.getLogger("vsd")

# ── Flask App ────────────────────────────────────────────────────────────────

app = Flask(__name__)

# Only the VSD browser extension (and MULA itself, which sends no Origin header) may
# talk to the server. Without this, any web page open in the browser could drive it,
# e.g. POST /open to launch a local executable. Configuration changes additionally
# require the admin token (see _has_admin_token).
_ALLOWED_ORIGIN_RE = re.compile(r"^(chrome-extension|moz-extension)://[A-Za-z0-9._-]+$")
_LOOPBACK_HOSTS = frozenset({"127.0.0.1", "localhost", "[::1]"})

CORS(app, resources={r"/*": {"origins": _ALLOWED_ORIGIN_RE}})


def _is_loopback_bind() -> bool:
    return SERVER_HOST in ("127.0.0.1", "localhost", "::1")


def _host_without_port(host: str) -> str:
    host = host.lower()
    if host.startswith("["):
        end = host.find("]")
        return host[:end + 1] if end != -1 else host
    return host.rsplit(":", 1)[0]


def _json_object() -> dict | None:
    """The request body as a JSON object, or None if it is missing, invalid or not an object."""
    data = request.get_json(force=True, silent=True)
    return data if isinstance(data, dict) else None


def _has_admin_token() -> bool:
    """True if the request carries the per-launch admin token MULA was started with.

    With no token configured, nobody is authorized: an unset or empty
    VSD_ADMIN_TOKEN disables configuration changes instead of opening them up.
    """
    if not ADMIN_TOKEN:
        logger.warning(f"Refused {request.method} {request.path}: no admin token configured")
        return False
    supplied = request.headers.get(ADMIN_TOKEN_HEADER, "")
    if not supplied or not hmac.compare_digest(supplied.encode(), ADMIN_TOKEN.encode()):
        logger.warning(f"Refused {request.method} {request.path}: missing or wrong admin token")
        return False
    return True


@app.before_request
def _reject_untrusted_callers():
    origin = request.headers.get("Origin")
    if origin is not None and not _ALLOWED_ORIGIN_RE.match(origin):
        logger.warning(f"Rejected request from origin {origin!r} to {request.path}")
        return jsonify({"error": "Forbidden origin"}), 403

    # Guard against DNS rebinding: a loopback-only server should only ever be
    # addressed by a loopback host name.
    if _is_loopback_bind():
        host = _host_without_port(request.host or "")
        if host not in _LOOPBACK_HOSTS:
            logger.warning(f"Rejected request with Host {request.host!r} to {request.path}")
            return jsonify({"error": "Forbidden host"}), 403
    return None

# ── Job Tracking ─────────────────────────────────────────────────────────────

_jobs: dict = {}
_jobs_lock = threading.Lock()
# Every file this server has produced; /open may open these even after their job
# has expired or the download directory has changed.
_produced_outputs: set[str] = set()


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


def _is_http_url(url: str) -> bool:
    return urllib.parse.urlsplit(url).scheme.lower() in ("http", "https")


_REDIRECT_CODES = frozenset({301, 302, 303, 307, 308})
_DEFAULT_PORTS = {"http": 80, "https": 443}
# A request on a kept-alive connection can fail like this when the server has
# already closed it while idle; that is retried once on a fresh connection.
_STALE_CONNECTION_ERRORS = (ConnectionResetError, ConnectionAbortedError, BrokenPipeError)


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Hands 3xx responses back to _fetch_once, which follows them itself so that
    every hop gets the http(s) scheme check and the cleaned headers."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


# Like urlopen (proxy-aware, fresh connection per request) but without following redirects.
_URLLIB_OPENER = urllib.request.build_opener(_NoRedirectHandler)


def _urllib_request(url: str, headers: dict) -> tuple[int, str, object, bytes]:
    """One GET through urllib. Returns (status, reason, headers, body) for 2xx and 3xx
    responses and raises urllib.error.HTTPError for any other status."""
    req = urllib.request.Request(url, headers=headers)
    try:
        with _URLLIB_OPENER.open(req, timeout=FETCH_TIMEOUT) as resp:
            return resp.status, resp.reason, resp.headers, resp.read()
    except urllib.error.HTTPError as e:
        if e.code in _REDIRECT_CODES:
            e.close()
            return e.code, e.reason, e.headers, b""
        raise


class _ConnectionPool:
    """Kept-alive http.client connections, shared by the workers of one download.

    A worker checks a connection out for a single request and returns it
    afterwards, so connections are reused across segments without ever being
    used by two threads at once. Hosts that must be reached through a proxy are
    reported by uses_proxy(); callers send those requests through urllib, which
    honours the proxy settings (http.client does not).
    """

    def __init__(self, proxies: dict | None = None):
        self._lock = threading.Lock()
        self._idle: dict[tuple, list] = {}
        self._closed = False
        self._proxies = urllib.request.getproxies() if proxies is None else proxies
        self._proxy_cache: dict[tuple, bool] = {}
        self._ssl_context = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def uses_proxy(self, url: str) -> bool:
        parts = urllib.parse.urlsplit(url)
        key = (parts.scheme.lower(), parts.hostname or "")
        with self._lock:
            cached = self._proxy_cache.get(key)
        if cached is not None:
            return cached
        if key[0] not in self._proxies:
            result = False
        else:
            try:
                result = not urllib.request.proxy_bypass(key[1])
            except Exception:
                result = True
        with self._lock:
            self._proxy_cache[key] = result
        return result

    def _connect(self, scheme: str, host: str, port: int) -> http.client.HTTPConnection:
        if scheme == "https":
            with self._lock:
                if self._ssl_context is None:
                    self._ssl_context = ssl.create_default_context()
                context = self._ssl_context
            return http.client.HTTPSConnection(host, port, timeout=FETCH_TIMEOUT, context=context)
        return http.client.HTTPConnection(host, port, timeout=FETCH_TIMEOUT)

    def _checkout(self, key: tuple) -> tuple[http.client.HTTPConnection, bool]:
        """An idle connection for `key` (reused=True) or a new one (reused=False)."""
        with self._lock:
            idle = self._idle.get(key)
            if idle:
                return idle.pop(), True
        return self._connect(*key), False

    def _checkin(self, key: tuple, conn: http.client.HTTPConnection):
        with self._lock:
            if not self._closed:
                self._idle.setdefault(key, []).append(conn)
                return
        conn.close()

    def request(self, url: str, headers: dict) -> tuple[int, str, object, bytes]:
        """One GET on a kept-alive connection. Returns (status, reason, headers, body)
        for any status; redirects and errors are left to the caller."""
        parts = urllib.parse.urlsplit(url)
        scheme = parts.scheme.lower()
        key = (scheme, parts.hostname or "", parts.port or _DEFAULT_PORTS[scheme])
        target = (parts.path or "/") + (f"?{parts.query}" if parts.query else "")

        while True:
            conn, reused = self._checkout(key)
            try:
                conn.request("GET", target, headers=headers)
                resp = conn.getresponse()
                body = resp.read()
            except _STALE_CONNECTION_ERRORS:
                conn.close()
                if reused:
                    continue  # the server dropped the idle connection; try a fresh one
                raise
            except BaseException:
                conn.close()
                raise
            if resp.will_close:
                conn.close()
            else:
                self._checkin(key, conn)
            return resp.status, resp.reason, resp.headers, body

    def close(self):
        with self._lock:
            self._closed = True
            conns = [c for idle in self._idle.values() for c in idle]
            self._idle.clear()
        for conn in conns:
            conn.close()


def _fetch_once(url: str, headers: dict, pool: _ConnectionPool | None = None) -> bytes:
    """GET `url` once, following redirects. `headers` must already be cleaned.

    Requests go through `pool` (kept-alive connections) when one is given and the
    host is not behind a proxy, otherwise through urllib. Every hop, including
    each redirect target, must be http(s) and is sent with the same headers.
    """
    for _ in range(MAX_REDIRECTS + 1):
        if not _is_http_url(url):
            raise ValueError(f"Refusing to fetch non-HTTP URL: {url[:80]}")
        if pool is not None and not pool.uses_proxy(url):
            status, reason, resp_headers, body = pool.request(url, headers)
        else:
            status, reason, resp_headers, body = _urllib_request(url, headers)
        if 200 <= status < 300:
            return body
        location = resp_headers.get("Location") if status in _REDIRECT_CODES else None
        if not location:
            raise urllib.error.HTTPError(url, status, reason, resp_headers, None)
        url = urllib.parse.urljoin(url, location)
    raise urllib.error.URLError(f"Too many redirects (more than {MAX_REDIRECTS})")


def _fetch(
    url: str,
    headers: dict | None = None,
    retries: int = MAX_RETRIES,
    pool: _ConnectionPool | None = None,
) -> bytes:
    """Fetch a URL with retry logic.

    With a `pool`, the first attempt uses a kept-alive connection; after a
    connection-level failure the remaining attempts fall back to urllib (a fresh,
    proxy-aware connection per request). HTTP error statuses are retried on the
    same transport. A non-http(s) URL or redirect target is never retried.
    """
    req_headers = {"User-Agent": _DEFAULT_UA, **_clean_headers(headers)}
    retries = max(1, retries)

    last_err = None
    for attempt in range(1, retries + 1):
        try:
            return _fetch_once(url, req_headers, pool)
        except ValueError:
            raise
        except Exception as e:
            last_err = e
            if pool is not None and not isinstance(e, urllib.error.HTTPError):
                pool = None
            if attempt < retries:
                logger.warning(f"Fetch attempt {attempt}/{retries} failed for {url[:80]}: {e}")
    raise last_err


def _extract_resolution(m3u8_url: str, segments: list[str]) -> str:
    text = m3u8_url + " " + " ".join(segments[:5])
    match = re.search(r"(\d{3,4}[Pp])", text)
    return match.group(1).upper() if match else ""


def _sanitize_filename(title: str, max_len: int = 80) -> str:
    if not title:
        return ""
    name = re.sub(r'[<>:"/\\|?*]', "_", title)
    # Whitespace (incl. tabs/newlines) separates words; \x1c-\x1f also count as
    # whitespace in Python but are control characters, so they are dropped instead.
    name = re.sub(r"[^\S\x1c-\x1f]+", "_", name)
    name = re.sub(r"[\x00-\x1f]", "", name)
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
            segments.append(urllib.parse.urljoin(base_url, line))
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
    """Download all .ts segments to a temp directory, SEGMENT_WORKERS at a time.

    Returns the downloaded files in playlist order (seg_00000, seg_00001, ...).
    Each segment gets its own retries (_fetch). Once a segment has failed for good
    the job cannot succeed, so segments that have not started yet are cancelled
    and the returned list is shorter than `segments`; _run_download treats that
    as a failure.
    """
    total = len(segments)
    _set_job(job_id, total=total, current=0, stage="downloading")
    if not total:
        return []
    results: list[Path | None] = [None] * total
    completed = 0

    def download(i: int, url: str, pool: _ConnectionPool) -> Path:
        ext = Path(urllib.parse.urlparse(url).path).suffix or ".ts"
        seg_path = tmpdir / f"seg_{i:05d}{ext}"
        seg_path.write_bytes(_fetch(url, headers, pool=pool))
        return seg_path

    # The executor is shut down (all workers finished) before the pool is closed.
    with _ConnectionPool() as pool, ThreadPoolExecutor(
        max_workers=min(SEGMENT_WORKERS, total), thread_name_prefix=f"vsd-{job_id}"
    ) as executor:
        futures = {executor.submit(download, i, url, pool): i for i, url in enumerate(segments)}
        failed = False
        for future in as_completed(futures):
            i = futures[future]
            if future.cancelled():
                continue
            try:
                results[i] = future.result()
            except Exception as e:
                logger.error(f"Segment {i+1}/{total} failed: {e}")
                if not failed:
                    failed = True
                    for other in futures:
                        other.cancel()
                continue
            completed += 1
            _set_job(job_id, current=completed)
            if completed % 20 == 0 or completed == total:
                logger.info(f"Progress: {completed}/{total} segments downloaded")

    return [p for p in results if p is not None]


def _mux_with_ffmpeg(seg_files: list[Path], output_path: Path, metadata: dict | None = None):
    """Concatenate .ts segments into a single MP4 using ffmpeg."""
    concat_list = output_path.parent / f".{output_path.stem}_concat.txt"
    concat_list.write_text(
        "".join(f"file {_concat_quote(seg.as_posix())}\n" for seg in seg_files),
        encoding="utf-8",
    )
    try:
        _run_ffmpeg(concat_list, output_path, metadata)
    finally:
        concat_list.unlink(missing_ok=True)
    logger.info(f"Output: {output_path.name} ({output_path.stat().st_size // 1024} KB)")


def _concat_quote(path: str) -> str:
    """Quote a path for ffmpeg's concat demuxer (a ' inside quotes is written as '\\'')."""
    return "'" + path.replace("'", "'\\''") + "'"


def _run_ffmpeg(concat_list: Path, output_path: Path, metadata: dict | None):
    """Remux the segments listed in `concat_list` into `output_path`."""
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


def _build_output_name(title: str, resolution: str, job_id: str) -> str:
    parts = []
    if title:
        parts.append(title)
    if resolution:
        parts.append(resolution)
    parts.append(job_id)
    return "_".join(parts) + ".mp4"


# Request metadata key -> ffmpeg metadata key (the extension popup shows the same fields).
METADATA_KEYS = {
    "pageUrl": "comment",
    "channel": "artist",
    "categories": "genre",
    "tags": "keywords",
    "actors": "album_artist",
    "uploadDate": "date",
}


def _build_metadata(title: str, raw: dict) -> dict:
    """Map request metadata to ffmpeg metadata keys; lists are joined with commas."""
    m = {"title": title} if title else {}
    for key, ffmpeg_key in METADATA_KEYS.items():
        value = raw.get(key)
        if value:
            m[ffmpeg_key] = ", ".join(map(str, value)) if isinstance(value, list) else str(value)
    return m


def _run_download(job_id: str, url: str, title: str, headers: dict | None, raw_metadata: dict):
    """Main download pipeline — runs in a background thread."""
    safe_title = _sanitize_filename(title)
    logger.info(f"Job {job_id}: starting download — {safe_title or url[:80]}")

    tmpdir = None
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
        if len(seg_files) < len(segments):
            raise ValueError(
                f"only {len(seg_files)} of {len(segments)} segments were downloaded; "
                "the video would have gaps, so nothing was saved"
            )

        # Determine output filename
        resolution = _extract_resolution(url, segments)
        output_name = _build_output_name(safe_title, resolution, job_id)
        output_path = DOWNLOAD_DIR / output_name

        # Mux
        _set_job(job_id, stage="muxing")
        metadata = _build_metadata(title, raw_metadata)
        _mux_with_ffmpeg(seg_files, output_path, metadata)

        # Auto-open if configured
        if AUTO_OPEN:
            _open_file(output_path)

        with _jobs_lock:
            _produced_outputs.add(os.path.normcase(str(output_path.resolve())))
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
    finally:
        if tmpdir is not None:
            shutil.rmtree(tmpdir, ignore_errors=True)


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
    data = _json_object()
    if data is None:
        return jsonify({"error": "Expected a JSON object"}), 400
    url = data.get("url")
    if not url:
        return jsonify({"error": "Missing 'url' field"}), 400
    if not isinstance(url, str) or not _is_http_url(url):
        return jsonify({"error": "Only http(s) URLs are supported"}), 400

    title = data.get("title", "")
    headers = data.get("headers")
    raw_metadata = {
        "pageUrl": data.get("pageUrl", ""),
        **(data.get("metadata") or {}),
    }

    job_id = uuid.uuid4().hex[:8]
    _set_job(job_id)
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
        tail = deque(f, maxlen=n)
    return jsonify({"log": "".join(tail)})


@app.route("/open", methods=["POST"])
def open_file_endpoint():
    data = _json_object()
    if data is None:
        return jsonify({"error": "Expected a JSON object"}), 400
    file_path = data.get("path")
    if not file_path:
        return jsonify({"error": "Missing 'path' field"}), 400

    path = Path(file_path).resolve()
    if not path.is_file():
        return jsonify({"error": "File not found"}), 404
    if not _may_open(path):
        logger.warning(f"Refused to open file outside the download directory: {path}")
        return jsonify({"error": "Only downloaded files can be opened"}), 403

    _open_file(path)
    return jsonify({"ok": True})


def _may_open(path: Path) -> bool:
    """Only open files that VSD downloaded: never arbitrary executables."""
    with _jobs_lock:
        if os.path.normcase(str(path)) in _produced_outputs:
            return True
    try:
        return path.is_relative_to(DOWNLOAD_DIR.resolve())
    except OSError:
        return False


@app.route("/config/download_dir", methods=["GET", "POST"])
def config_download_dir():
    global DOWNLOAD_DIR
    if request.method == "GET":
        return jsonify({"download_dir": str(DOWNLOAD_DIR)})

    # The token is the only authority: MULA sends it (and no Origin). An allowed
    # extension Origin with the right token is accepted too, but extensions are
    # never given the token; web origins never get here (_reject_untrusted_callers).
    if not _has_admin_token():
        return jsonify({"error": "The download directory can only be changed by MULA"}), 403

    data = _json_object()
    if data is None:
        return jsonify({"error": "Expected a JSON object"}), 400
    new_dir = data.get("path")
    if not new_dir:
        return jsonify({"error": "Missing 'path' field"}), 400

    new_path = Path(new_dir)
    new_path.mkdir(parents=True, exist_ok=True)
    DOWNLOAD_DIR = new_path
    logger.info(f"Download dir changed to: {DOWNLOAD_DIR}")
    return jsonify({"ok": True, "download_dir": str(DOWNLOAD_DIR)})


# ── Entry Point ──────────────────────────────────────────────────────────────

if __name__ == "__main__":
    # Suppress Werkzeug startup banner
    logging.getLogger("werkzeug").setLevel(logging.ERROR)

    logger.info(f"VSD Server starting on http://{SERVER_HOST}:{SERVER_PORT}")
    logger.info(f"Download dir: {DOWNLOAD_DIR}")
    logger.info(f"ffmpeg: {FFMPEG_PATH}")
    if not ADMIN_TOKEN:
        logger.warning("VSD_ADMIN_TOKEN is not set: changing the download directory is disabled")
    app.run(host=SERVER_HOST, port=SERVER_PORT, debug=False, threaded=True)
