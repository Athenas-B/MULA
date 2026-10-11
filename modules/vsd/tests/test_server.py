"""Unit tests for modules/vsd/server.py.

Run from modules/vsd:
    PYTHONDONTWRITEBYTECODE=1 python -m unittest discover -s tests -v

server.py creates DOWNLOAD_DIR and LOG_DIR and attaches a log FileHandler at
import time, so the environment is pointed at temporary directories *before*
the module is imported. Nothing here touches the network, ffmpeg, the real
~/Downloads/VSD folder or modules/vsd/logs.
"""

import http.server
import logging
import os
import shutil
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from time import time
from unittest import mock

# ── Isolated environment (must happen before `import server`) ───────────────

_TMP_ROOT = Path(tempfile.mkdtemp(prefix="vsd_tests_"))
_TMP_DOWNLOADS = _TMP_ROOT / "downloads"
_TMP_LOGS = _TMP_ROOT / "logs"

# Set explicitly so modules/vsd/.env (load_dotenv does not override) cannot leak in.
os.environ["VSD_DOWNLOAD_DIR"] = str(_TMP_DOWNLOADS)
os.environ["VSD_LOG_DIR"] = str(_TMP_LOGS)
os.environ["VSD_HOST"] = "127.0.0.1"
os.environ["VSD_PORT"] = "8765"
os.environ["VSD_AUTO_OPEN"] = "0"
os.environ["VSD_MAX_RETRIES"] = "3"
os.environ["VSD_FFMPEG"] = "ffmpeg-test-binary"
os.environ["VSD_SEGMENT_WORKERS"] = "6"
ADMIN_TOKEN = "test-admin-token-0123456789abcdef"
os.environ["VSD_ADMIN_TOKEN"] = ADMIN_TOKEN

_VSD_DIR = Path(__file__).resolve().parent.parent
if str(_VSD_DIR) not in sys.path:
    sys.path.insert(0, str(_VSD_DIR))

import server  # noqa: E402

# Detach the import-time handlers (stdout + the temp log file) so test output
# stays quiet and the temp directory can be removed on Windows.
_root_logger = logging.getLogger()
_IMPORT_LOG_FILE_HANDLERS = [
    h for h in _root_logger.handlers if isinstance(h, logging.FileHandler)
]
_IMPORT_LOG_PATHS = [Path(h.baseFilename) for h in _IMPORT_LOG_FILE_HANDLERS]
for _h in list(_root_logger.handlers):
    _root_logger.removeHandler(_h)
    _h.close()
_root_logger.addHandler(logging.NullHandler())


def tearDownModule():
    shutil.rmtree(_TMP_ROOT, ignore_errors=True)


EXT_ORIGIN = "chrome-extension://abcdefghijklmnopabcdefghijklmnop"
MOZ_ORIGIN = "moz-extension://0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b"


class _ServerStateMixin:
    """Resets module-level mutable state around every test."""

    def setUp(self):
        super().setUp()
        self._orig_download_dir = server.DOWNLOAD_DIR
        self._orig_log_file = server.LOG_FILE
        self._orig_host = server.SERVER_HOST
        self._orig_auto_open = server.AUTO_OPEN
        self._orig_admin_token = server.ADMIN_TOKEN
        self._orig_segment_workers = server.SEGMENT_WORKERS
        with server._jobs_lock:
            server._jobs.clear()
            server._produced_outputs.clear()
        self.tmp = Path(tempfile.mkdtemp(prefix="case_", dir=_TMP_ROOT))
        self.download_dir = self.tmp / "downloads"
        self.download_dir.mkdir()
        server.DOWNLOAD_DIR = self.download_dir

    def tearDown(self):
        server.DOWNLOAD_DIR = self._orig_download_dir
        server.LOG_FILE = self._orig_log_file
        server.SERVER_HOST = self._orig_host
        server.AUTO_OPEN = self._orig_auto_open
        server.ADMIN_TOKEN = self._orig_admin_token
        server.SEGMENT_WORKERS = self._orig_segment_workers
        with server._jobs_lock:
            server._jobs.clear()
            server._produced_outputs.clear()
        shutil.rmtree(self.tmp, ignore_errors=True)
        super().tearDown()


# ── Import-time isolation ───────────────────────────────────────────────────

class ImportIsolationTests(unittest.TestCase):
    def test_dirs_come_from_env(self):
        self.assertEqual(Path(os.environ["VSD_DOWNLOAD_DIR"]), _TMP_DOWNLOADS)
        self.assertTrue(_TMP_DOWNLOADS.is_dir())
        self.assertEqual(server.LOG_DIR, _TMP_LOGS)
        self.assertEqual(server.LOG_FILE, _TMP_LOGS / "vsd.log")
        self.assertTrue(_TMP_LOGS.is_dir())

    def test_log_handler_pointed_at_temp_dir(self):
        self.assertTrue(_IMPORT_LOG_PATHS, "expected a FileHandler from basicConfig")
        for p in _IMPORT_LOG_PATHS:
            self.assertEqual(p.parent.resolve(), _TMP_LOGS.resolve())

    def test_ffmpeg_and_host_from_env(self):
        self.assertEqual(server.FFMPEG_PATH, "ffmpeg-test-binary")
        self.assertEqual(server.SERVER_HOST, "127.0.0.1")
        self.assertFalse(server.AUTO_OPEN)


# ── Pure helpers ────────────────────────────────────────────────────────────

class CleanHeadersTests(unittest.TestCase):
    def test_none_and_empty(self):
        self.assertEqual(server._clean_headers(None), {})
        self.assertEqual(server._clean_headers({}), {})

    def test_strips_hop_by_hop_headers_case_insensitively(self):
        headers = {
            "Host": "example.com",
            "Accept-Encoding": "gzip",
            "CONNECTION": "keep-alive",
            "content-length": "12",
            "Upgrade-Insecure-Requests": "1",
            "Referer": "https://example.com/page",
            "Cookie": "a=b",
        }
        self.assertEqual(
            server._clean_headers(headers),
            {"Referer": "https://example.com/page", "Cookie": "a=b"},
        )

    def test_does_not_mutate_input(self):
        headers = {"Host": "x", "Referer": "y"}
        server._clean_headers(headers)
        self.assertEqual(headers, {"Host": "x", "Referer": "y"})


class IsHttpUrlTests(unittest.TestCase):
    def test_accepts_http_and_https(self):
        for url in ("http://a.com/x.m3u8", "https://a.com/x", "HTTPS://A.COM/", "HtTp://a"):
            with self.subTest(url=url):
                self.assertTrue(server._is_http_url(url))

    def test_rejects_other_schemes(self):
        for url in (
            "file:///C:/Windows/System32/calc.exe",
            "ftp://a.com/x",
            "javascript:alert(1)",
            "data:text/plain,hi",
            "//a.com/x",
            "/relative/path",
            "",
            "C:\\Windows\\notepad.exe",
        ):
            with self.subTest(url=url):
                self.assertFalse(server._is_http_url(url))


class ResolveUrlTests(unittest.TestCase):
    def test_absolute_urls_untouched(self):
        self.assertEqual(
            server._resolve_url("https://a.com/v/index.m3u8", "https://cdn.b.com/s1.ts"),
            "https://cdn.b.com/s1.ts",
        )
        self.assertEqual(
            server._resolve_url("https://a.com/v/index.m3u8", "http://c.com/s1.ts"),
            "http://c.com/s1.ts",
        )

    def test_relative_segment(self):
        self.assertEqual(
            server._resolve_url("https://a.com/v/720/index.m3u8", "seg1.ts"),
            "https://a.com/v/720/seg1.ts",
        )

    def test_root_relative_and_parent(self):
        base = "https://a.com/v/720/index.m3u8?token=abc"
        self.assertEqual(server._resolve_url(base, "/s/seg1.ts"), "https://a.com/s/seg1.ts")
        self.assertEqual(server._resolve_url(base, "../seg1.ts"), "https://a.com/v/seg1.ts")

    def test_protocol_relative(self):
        self.assertEqual(
            server._resolve_url("https://a.com/v/index.m3u8", "//cdn.a.com/seg.ts"),
            "https://cdn.a.com/seg.ts",
        )


class ExtractResolutionTests(unittest.TestCase):
    def test_from_playlist_url(self):
        self.assertEqual(server._extract_resolution("https://a.com/hls/720p/index.m3u8", []), "720P")

    def test_from_segments(self):
        segs = ["https://a.com/seg_1080p_001.ts", "https://a.com/seg_1080p_002.ts"]
        self.assertEqual(server._extract_resolution("https://a.com/index.m3u8", segs), "1080P")

    def test_uppercase_input(self):
        self.assertEqual(server._extract_resolution("https://a.com/480P/x.m3u8", []), "480P")

    def test_only_first_five_segments_considered(self):
        segs = ["https://a.com/s%d.ts" % i for i in range(5)] + ["https://a.com/360p/s5.ts"]
        self.assertEqual(server._extract_resolution("https://a.com/index.m3u8", segs), "")

    def test_none_found(self):
        self.assertEqual(server._extract_resolution("https://a.com/index.m3u8", ["s1.ts"]), "")

    def test_url_wins_over_segments(self):
        self.assertEqual(
            server._extract_resolution("https://a.com/720p/i.m3u8", ["https://a.com/1080p/s.ts"]),
            "720P",
        )


class SanitizeFilenameTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(server._sanitize_filename(""), "")
        self.assertEqual(server._sanitize_filename(None), "")

    def test_replaces_reserved_chars(self):
        self.assertEqual(server._sanitize_filename('a<b>c:d"e/f\\g|h?i*j'), "a_b_c_d_e_f_g_h_i_j")

    def test_strips_control_chars(self):
        self.assertEqual(server._sanitize_filename("ab\x00c\x1fd\x07"), "abcd")

    def test_spaces_collapsed_to_underscore(self):
        self.assertEqual(server._sanitize_filename("My   Cool Video"), "My_Cool_Video")

    def test_tab_and_newline_act_as_word_separators(self):
        self.assertEqual(server._sanitize_filename("My  Cool\tVideo\nTitle"), "My_Cool_Video_Title")

    def test_strips_leading_trailing_dots_spaces_underscores(self):
        self.assertEqual(server._sanitize_filename("  ..Hello World.. "), "Hello_World")
        self.assertEqual(server._sanitize_filename("___x___"), "x")

    def test_only_unsafe_chars_becomes_empty(self):
        self.assertEqual(server._sanitize_filename("///"), "")
        self.assertEqual(server._sanitize_filename("..."), "")

    def test_no_path_separators_survive(self):
        out = server._sanitize_filename("..\\..\\Windows/System32/evil")
        self.assertNotIn("/", out)
        self.assertNotIn("\\", out)
        self.assertFalse(out.startswith("."))

    def test_truncates_on_word_boundary(self):
        title = " ".join(["word"] * 40)  # 199 chars
        out = server._sanitize_filename(title)
        self.assertLessEqual(len(out), 80)
        self.assertFalse(out.endswith("_"))
        self.assertTrue(out.endswith("word"))

    def test_truncates_hard_without_separator(self):
        out = server._sanitize_filename("x" * 200)
        self.assertEqual(out, "x" * 80)

    def test_custom_max_len(self):
        # Cut lands right after a separator -> trailing "_" part dropped.
        self.assertEqual(server._sanitize_filename("abc def ghi", max_len=8), "abc_def")
        # Cut lands mid-word -> partial word dropped.
        self.assertEqual(server._sanitize_filename("abc def ghi", max_len=9), "abc_def")
        self.assertEqual(server._sanitize_filename("abc def ghi", max_len=6), "abc")
        # Short enough -> untouched.
        self.assertEqual(server._sanitize_filename("abc def ghi", max_len=11), "abc_def_ghi")

    def test_unicode_preserved(self):
        self.assertEqual(server._sanitize_filename("Příliš žluťoučký kůň"), "Příliš_žluťoučký_kůň")


class ParseM3u8Tests(unittest.TestCase):
    BASE = "https://cdn.example.com/hls/720p/index.m3u8"

    def test_parses_segments_and_skips_tags(self):
        content = (
            b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:10\n"
            b"#EXTINF:10.0,\nseg0.ts\n"
            b"#EXTINF:10.0,\n/abs/seg1.ts\n"
            b"#EXTINF:10.0,\nhttps://other.com/seg2.ts?token=abc\n"
            b"\n   \n#EXT-X-ENDLIST\n"
        )
        self.assertEqual(
            server._parse_m3u8(self.BASE, content),
            [
                "https://cdn.example.com/hls/720p/seg0.ts",
                "https://cdn.example.com/abs/seg1.ts",
                "https://other.com/seg2.ts?token=abc",
            ],
        )

    def test_crlf_and_whitespace(self):
        content = b"#EXTM3U\r\n#EXTINF:4,\r\n  a.ts  \r\n"
        self.assertEqual(
            server._parse_m3u8(self.BASE, content),
            ["https://cdn.example.com/hls/720p/a.ts"],
        )

    def test_ignores_non_ts_entries(self):
        # Master playlists / fMP4 segments are not .ts and are ignored.
        content = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\n720p.m3u8\ninit.mp4\nseg.m4s\n"
        self.assertEqual(server._parse_m3u8(self.BASE, content), [])

    def test_empty_and_invalid_utf8(self):
        self.assertEqual(server._parse_m3u8(self.BASE, b""), [])
        self.assertEqual(
            server._parse_m3u8(self.BASE, b"\xff\xfe#EXTM3U\nseg\xff1.ts\n"),
            ["https://cdn.example.com/hls/720p/seg1.ts"],
        )


class BuildOutputNameTests(unittest.TestCase):
    def test_all_parts(self):
        self.assertEqual(server._build_output_name("Title", "720P", "abcd1234"), "Title_720P_abcd1234.mp4")

    def test_without_resolution(self):
        self.assertEqual(server._build_output_name("Title", "", "abcd1234"), "Title_abcd1234.mp4")

    def test_without_title(self):
        self.assertEqual(server._build_output_name("", "1080P", "abcd1234"), "1080P_abcd1234.mp4")

    def test_only_job_id(self):
        self.assertEqual(server._build_output_name("", "", "abcd1234"), "abcd1234.mp4")


class BuildMetadataTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(server._build_metadata("", {}), {})

    def test_full_mapping_with_lists(self):
        raw = {
            "pageUrl": "https://site/page",
            "channel": "Chan",
            "categories": ["a", "b"],
            "tags": ["t1", "t2"],
            "actors": ["x", "y"],
            "uploadDate": "2026-01-02",
        }
        self.assertEqual(
            server._build_metadata("My Title", raw),
            {
                "title": "My Title",
                "comment": "https://site/page",
                "artist": "Chan",
                "genre": "a, b",
                "keywords": "t1, t2",
                "album_artist": "x, y",
                "date": "2026-01-02",
            },
        )

    def test_scalar_values_stringified(self):
        m = server._build_metadata("", {"categories": "cat", "tags": 5, "actors": "solo"})
        self.assertEqual(m, {"genre": "cat", "keywords": "5", "album_artist": "solo"})

    def test_falsy_values_skipped(self):
        raw = {"pageUrl": "", "channel": None, "categories": [], "tags": "", "actors": None}
        self.assertEqual(server._build_metadata("", raw), {})


class JobTrackingTests(_ServerStateMixin, unittest.TestCase):
    def test_set_job_creates_defaults(self):
        server._set_job("j1")
        job = server._jobs["j1"]
        self.assertEqual(job["status"], "running")
        self.assertEqual(job["current"], 0)
        self.assertEqual(job["total"], 0)
        self.assertEqual(job["stage"], "starting")
        self.assertIsNone(job["output"])
        self.assertIsNone(job["error"])
        self.assertIsNone(job["finished_at"])
        self.assertIsInstance(job["started_at"], float)

    def test_set_job_updates_existing(self):
        server._set_job("j1", total=10)
        started = server._jobs["j1"]["started_at"]
        server._set_job("j1", current=4, stage="downloading")
        job = server._jobs["j1"]
        self.assertEqual((job["current"], job["total"], job["stage"]), (4, 10, "downloading"))
        self.assertEqual(job["started_at"], started)

    def test_cleanup_removes_only_expired_finished_jobs(self):
        now = time()
        server._set_job("old_done", status="done", finished_at=now - server.JOB_TTL_SECONDS - 5)
        server._set_job("recent_done", status="done", finished_at=now - 10)
        server._set_job("running")
        server._set_job("old_running_no_finish", started_at=now - 10 * server.JOB_TTL_SECONDS)
        server._cleanup_jobs()
        self.assertEqual(
            set(server._jobs),
            {"recent_done", "running", "old_running_no_finish"},
        )


class MayOpenTests(_ServerStateMixin, unittest.TestCase):
    def test_file_inside_download_dir(self):
        f = self.download_dir / "sub" / "video.mp4"
        f.parent.mkdir()
        f.write_bytes(b"x")
        self.assertTrue(server._may_open(f.resolve()))

    def test_file_outside_download_dir(self):
        f = self.tmp / "outside.exe"
        f.write_bytes(b"x")
        self.assertFalse(server._may_open(f.resolve()))

    def test_sibling_prefix_dir_not_allowed(self):
        # downloads_evil shares a string prefix with downloads but is not inside it.
        evil = self.tmp / "downloads_evil" / "x.exe"
        evil.parent.mkdir()
        evil.write_bytes(b"x")
        self.assertFalse(server._may_open(evil.resolve()))

    def test_produced_output_outside_download_dir(self):
        f = self.tmp / "old_dir" / "video.mp4"
        f.parent.mkdir()
        f.write_bytes(b"x")
        server._produced_outputs.add(os.path.normcase(str(f.resolve())))
        self.assertTrue(server._may_open(f.resolve()))


def _urllib_resp(data, status=200, reason="OK", headers=None):
    """A stand-in for what _URLLIB_OPENER.open returns (a context manager)."""
    cm = mock.MagicMock()
    resp = cm.__enter__.return_value
    resp.status, resp.reason, resp.headers = status, reason, headers or {}
    resp.read.return_value = data
    return cm


class FetchTests(unittest.TestCase):
    """_fetch without a connection pool: every request goes through urllib."""

    def _open(self, **kw):
        return mock.patch.object(server._URLLIB_OPENER, "open", **kw)

    def test_rejects_non_http_schemes_without_network(self):
        with self._open() as urlopen:
            for url in ("file:///C:/secret.txt", "ftp://a/b", "javascript:x", "C:\\x.ts"):
                with self.subTest(url=url):
                    with self.assertRaises(ValueError):
                        server._fetch(url)
            urlopen.assert_not_called()

    def test_success_sends_ua_and_cleaned_headers(self):
        with self._open(return_value=_urllib_resp(b"OK")) as urlopen:
            data = server._fetch("https://a.com/x.ts", {"Host": "evil", "Referer": "https://a.com/"})
        self.assertEqual(data, b"OK")
        req = urlopen.call_args.args[0]
        self.assertEqual(req.full_url, "https://a.com/x.ts")
        self.assertEqual(req.get_header("User-agent"), server._DEFAULT_UA)
        self.assertEqual(req.get_header("Referer"), "https://a.com/")
        self.assertIsNone(req.get_header("Host"))
        self.assertEqual(urlopen.call_args.kwargs.get("timeout"), 30)

    def test_caller_can_override_user_agent(self):
        with self._open(return_value=_urllib_resp(b"")) as urlopen:
            server._fetch("https://a.com/x", {"User-Agent": "Custom/1.0"})
        self.assertEqual(urlopen.call_args.args[0].get_header("User-agent"), "Custom/1.0")

    def test_retries_then_succeeds(self):
        with self._open(side_effect=[OSError("boom"), OSError("boom2"), _urllib_resp(b"data")]) as urlopen:
            self.assertEqual(server._fetch("https://a.com/x", retries=3), b"data")
        self.assertEqual(urlopen.call_count, 3)

    def test_raises_last_error_after_retries(self):
        errors = [OSError("e1"), OSError("e2")]
        with self._open(side_effect=errors) as urlopen:
            with self.assertRaises(OSError) as ctx:
                server._fetch("https://a.com/x", retries=2)
        self.assertIs(ctx.exception, errors[1])
        self.assertEqual(urlopen.call_count, 2)

    def test_zero_retries_still_attempts_once(self):
        with self._open(return_value=_urllib_resp(b"data")):
            self.assertEqual(server._fetch("https://a.com/x", retries=0), b"data")

    def test_http_error_status_is_retried(self):
        err = urllib.error.HTTPError("https://a.com/x", 503, "Unavailable", {}, None)
        with self._open(side_effect=[err, _urllib_resp(b"data")]) as urlopen:
            self.assertEqual(server._fetch("https://a.com/x", retries=3), b"data")
        self.assertEqual(urlopen.call_count, 2)

    def test_urllib_redirect_is_followed_by_fetch_with_the_same_headers(self):
        redirect = urllib.error.HTTPError(
            "https://a.com/x", 302, "Found", {"Location": "https://cdn.b.com/y.ts"}, None
        )
        with self._open(side_effect=[redirect, _urllib_resp(b"data")]) as urlopen:
            data = server._fetch("https://a.com/x", {"Referer": "r", "Connection": "keep-alive"})
        self.assertEqual(data, b"data")
        first, second = (c.args[0] for c in urlopen.call_args_list)
        self.assertEqual(second.full_url, "https://cdn.b.com/y.ts")
        self.assertEqual(second.header_items(), first.header_items())
        self.assertEqual(second.get_header("Referer"), "r")
        self.assertIsNone(second.get_header("Connection"))

    def test_redirect_to_non_http_scheme_is_refused_and_not_retried(self):
        for location in ("file:///C:/Windows/win.ini", "ftp://a.com/x.ts", "javascript:alert(1)"):
            redirect = urllib.error.HTTPError("https://a.com/x", 302, "Found", {"Location": location}, None)
            with self.subTest(location=location), self._open(side_effect=[redirect]) as urlopen:
                with self.assertRaises(ValueError):
                    server._fetch("https://a.com/x", retries=3)
                self.assertEqual(urlopen.call_count, 1)

    def test_too_many_redirects(self):
        def loop(req, timeout):
            raise urllib.error.HTTPError(req.full_url, 302, "Found", {"Location": "/again"}, None)

        with self._open(side_effect=loop) as urlopen:
            with self.assertRaises(urllib.error.URLError) as ctx:
                server._fetch("https://a.com/x", retries=1)
        self.assertIn("Too many redirects", str(ctx.exception))
        self.assertEqual(urlopen.call_count, server.MAX_REDIRECTS + 1)

    def test_redirect_without_location_is_an_error(self):
        redirect = urllib.error.HTTPError("https://a.com/x", 302, "Found", {}, None)
        with self._open(side_effect=[redirect]):
            with self.assertRaises(urllib.error.HTTPError) as ctx:
                server._fetch("https://a.com/x", retries=1)
        self.assertEqual(ctx.exception.code, 302)

    def test_opener_does_not_follow_redirects_itself(self):
        handlers = [h for h in server._URLLIB_OPENER.handlers
                    if isinstance(h, urllib.request.HTTPRedirectHandler)]
        self.assertEqual(len(handlers), 1)
        self.assertIsInstance(handlers[0], server._NoRedirectHandler)
        self.assertIsNone(handlers[0].redirect_request(None, None, 302, "Found", {}, "file:///x"))


class _FakeResponse:
    def __init__(self, body=b"ok", status=200, headers=None, will_close=False):
        self.body, self.status, self.reason = body, status, "OK"
        self.headers = headers or {}
        self.will_close = will_close

    def read(self):
        return self.body


class _FakeConnection:
    """Records requests; `responses` items are _FakeResponse or exceptions to raise."""

    def __init__(self, key, responses):
        self.key = key
        self.responses = list(responses)
        self.requests = []
        self.closed = False

    def request(self, method, target, headers=None):
        self.requests.append((method, target, headers))
        item = self.responses.pop(0) if self.responses else _FakeResponse()
        if isinstance(item, BaseException):
            raise item
        self._pending = item

    def getresponse(self):
        return self._pending

    def close(self):
        self.closed = True


class ConnectionPoolTests(unittest.TestCase):
    def setUp(self):
        self.pool = server._ConnectionPool(proxies={})
        self.addCleanup(self.pool.close)
        self.created = []
        self.next_responses = []  # one list of responses per connection to be created

        def connect(scheme, host, port):
            conn = _FakeConnection((scheme, host, port),
                                   self.next_responses.pop(0) if self.next_responses else [])
            self.created.append(conn)
            return conn

        p = mock.patch.object(self.pool, "_connect", side_effect=connect)
        p.start()
        self.addCleanup(p.stop)

    def test_reuses_one_connection_per_host(self):
        for i in range(3):
            self.pool.request(f"https://cdn.a.com/s{i}.ts", {"Referer": "r"})
        self.assertEqual(len(self.created), 1)
        self.assertEqual(self.created[0].key, ("https", "cdn.a.com", 443))
        self.assertEqual([r[1] for r in self.created[0].requests], ["/s0.ts", "/s1.ts", "/s2.ts"])
        self.assertEqual(self.created[0].requests[0][2], {"Referer": "r"})
        self.assertFalse(self.created[0].closed)

    def test_separate_connections_per_scheme_host_and_port(self):
        for url in ("https://a.com/x", "http://a.com/x", "https://a.com:8443/x", "https://b.com/x",
                    "https://a.com/y"):
            self.pool.request(url, {})
        self.assertEqual(
            [c.key for c in self.created],
            [("https", "a.com", 443), ("http", "a.com", 80), ("https", "a.com", 8443),
             ("https", "b.com", 443)],
        )

    def test_request_target_keeps_query_and_drops_fragment(self):
        self.pool.request("https://a.com/p/s.ts?token=abc&x=1#frag", {})
        self.pool.request("https://a.com", {})
        self.assertEqual([r[1] for r in self.created[0].requests], ["/p/s.ts?token=abc&x=1", "/"])

    def test_returns_status_headers_and_body_for_any_status(self):
        self.next_responses = [[_FakeResponse(b"nope", status=404, headers={"X": "1"})]]
        status, _, headers, body = self.pool.request("https://a.com/x", {})
        self.assertEqual((status, headers, body), (404, {"X": "1"}, b"nope"))

    def test_connection_closed_when_server_will_close_it(self):
        self.next_responses = [[_FakeResponse(will_close=True)]]
        self.pool.request("https://a.com/x", {})
        self.pool.request("https://a.com/y", {})
        self.assertEqual(len(self.created), 2)
        self.assertTrue(self.created[0].closed)

    def test_stale_reused_connection_is_retried_on_a_fresh_one(self):
        self.next_responses = [[_FakeResponse(b"1"), ConnectionResetError("idle timeout")],
                               [_FakeResponse(b"2")]]
        self.assertEqual(self.pool.request("https://a.com/1", {})[3], b"1")
        self.assertEqual(self.pool.request("https://a.com/2", {})[3], b"2")
        self.assertEqual(len(self.created), 2)
        self.assertTrue(self.created[0].closed)

    def test_failure_on_a_fresh_connection_is_raised(self):
        self.next_responses = [[ConnectionResetError("refused")]]
        with self.assertRaises(ConnectionResetError):
            self.pool.request("https://a.com/x", {})
        self.assertEqual(len(self.created), 1)
        self.assertTrue(self.created[0].closed)

    def test_failed_connection_is_not_reused(self):
        self.next_responses = [[TimeoutError("slow")]]
        with self.assertRaises(TimeoutError):
            self.pool.request("https://a.com/x", {})
        self.pool.request("https://a.com/y", {})
        self.assertEqual(len(self.created), 2)
        self.assertTrue(self.created[0].closed)

    def test_close_closes_idle_connections_and_later_checkins(self):
        self.pool.request("https://a.com/x", {})
        self.pool.close()
        self.assertTrue(self.created[0].closed)
        self.pool.request("https://a.com/y", {})  # still works, but is not kept
        self.assertTrue(self.created[1].closed)

    def test_uses_proxy(self):
        self.assertFalse(server._ConnectionPool(proxies={}).uses_proxy("https://a.com/x"))
        pool = server._ConnectionPool(proxies={"https": "http://proxy:3128"})
        with mock.patch.object(server.urllib.request, "proxy_bypass", return_value=False):
            self.assertTrue(pool.uses_proxy("https://a.com/x"))
        self.assertFalse(pool.uses_proxy("http://a.com/x"))  # no proxy for http
        pool = server._ConnectionPool(proxies={"https": "http://proxy:3128"})
        with mock.patch.object(server.urllib.request, "proxy_bypass", return_value=True):
            self.assertFalse(pool.uses_proxy("https://intranet/x"))


class PooledFetchTests(unittest.TestCase):
    """_fetch / _fetch_once with a pool: transport choice, redirects and fallback."""

    def setUp(self):
        self.pool = mock.Mock(spec=server._ConnectionPool)
        self.pool.uses_proxy.return_value = False
        p = mock.patch.object(server._URLLIB_OPENER, "open")
        self.urlopen = p.start()
        self.addCleanup(p.stop)

    def test_pool_is_used_when_no_proxy_applies(self):
        self.pool.request.return_value = (200, "OK", {}, b"data")
        self.assertEqual(server._fetch("https://a.com/x", {"Referer": "r"}, pool=self.pool), b"data")
        url, headers = self.pool.request.call_args.args
        self.assertEqual(url, "https://a.com/x")
        self.assertEqual(headers, {"User-Agent": server._DEFAULT_UA, "Referer": "r"})
        self.urlopen.assert_not_called()

    def test_urllib_is_used_for_proxied_hosts(self):
        self.pool.uses_proxy.return_value = True
        self.urlopen.return_value = _urllib_resp(b"via proxy")
        self.assertEqual(server._fetch("https://a.com/x", pool=self.pool), b"via proxy")
        self.pool.request.assert_not_called()

    def test_transport_error_falls_back_to_urllib_for_remaining_attempts(self):
        self.pool.request.side_effect = ConnectionRefusedError("refused")
        self.urlopen.side_effect = [OSError("still down"), _urllib_resp(b"data")]
        self.assertEqual(server._fetch("https://a.com/x", retries=3, pool=self.pool), b"data")
        self.assertEqual(self.pool.request.call_count, 1)
        self.assertEqual(self.urlopen.call_count, 2)

    def test_http_error_status_keeps_using_the_pool(self):
        self.pool.request.side_effect = [(503, "Unavailable", {}, b""), (200, "OK", {}, b"data")]
        self.assertEqual(server._fetch("https://a.com/x", retries=3, pool=self.pool), b"data")
        self.assertEqual(self.pool.request.call_count, 2)
        self.urlopen.assert_not_called()

    def test_http_error_status_is_raised_after_retries(self):
        self.pool.request.return_value = (404, "Not Found", {}, b"")
        with self.assertRaises(urllib.error.HTTPError) as ctx:
            server._fetch("https://a.com/x", retries=2, pool=self.pool)
        self.assertEqual(ctx.exception.code, 404)
        self.assertEqual(self.pool.request.call_count, 2)

    def test_redirect_is_followed_on_the_pool_with_the_same_headers(self):
        self.pool.request.side_effect = [
            (301, "Moved", {"Location": "/v2/s.ts?sig=1"}, b""),
            (200, "OK", {}, b"data"),
        ]
        data = server._fetch("https://a.com/v1/s.ts", {"Host": "x", "Cookie": "c"}, pool=self.pool)
        self.assertEqual(data, b"data")
        (url1, h1), (url2, h2) = (c.args for c in self.pool.request.call_args_list)
        self.assertEqual(url2, "https://a.com/v2/s.ts?sig=1")
        self.assertEqual(h1, h2)
        self.assertEqual(h2, {"User-Agent": server._DEFAULT_UA, "Cookie": "c"})

    def test_redirect_to_non_http_scheme_is_refused(self):
        self.pool.request.return_value = (302, "Found", {"Location": "file:///etc/passwd"}, b"")
        with self.assertRaises(ValueError):
            server._fetch("https://a.com/x", retries=3, pool=self.pool)
        self.assertEqual(self.pool.request.call_count, 1)
        self.urlopen.assert_not_called()

    def test_redirect_to_a_proxied_host_switches_to_urllib(self):
        self.pool.uses_proxy.side_effect = lambda url: "proxied" in url
        self.pool.request.return_value = (302, "Found", {"Location": "https://proxied.com/s.ts"}, b"")
        self.urlopen.return_value = _urllib_resp(b"data")
        self.assertEqual(server._fetch("https://a.com/x", pool=self.pool), b"data")
        self.assertEqual(self.urlopen.call_args.args[0].full_url, "https://proxied.com/s.ts")


class OpenFileTests(unittest.TestCase):
    def test_windows_uses_startfile(self):
        with mock.patch.object(server.platform, "system", return_value="Windows"), \
                mock.patch.object(server.os, "startfile", create=True) as startfile:
            server._open_file(Path("C:/x/video.mp4"))
        startfile.assert_called_once_with(str(Path("C:/x/video.mp4")))

    def test_linux_uses_xdg_open(self):
        with mock.patch.object(server.platform, "system", return_value="Linux"), \
                mock.patch.object(server.sys, "platform", "linux"), \
                mock.patch.object(server.subprocess, "Popen") as popen:
            server._open_file(Path("/x/video.mp4"))
        self.assertEqual(popen.call_args.args[0][0], "xdg-open")

    def test_errors_are_swallowed(self):
        with mock.patch.object(server.platform, "system", return_value="Windows"), \
                mock.patch.object(server.os, "startfile", create=True, side_effect=OSError("nope")):
            server._open_file(Path("C:/x/video.mp4"))  # must not raise


# ── Download pipeline (network / ffmpeg mocked) ─────────────────────────────

class DownloadSegmentsTests(_ServerStateMixin, unittest.TestCase):
    WAIT = 5  # seconds; generous upper bound so a broken implementation fails instead of hanging

    def _segs(self, n):
        return [f"https://a.com/s{i}.ts" for i in range(n)]

    def test_downloads_and_tracks_progress(self):
        segs = ["https://a.com/s0.ts", "https://a.com/s1.ts?x=1", "https://a.com/s2"]
        calls = []

        def fake_fetch(url, headers=None, **kw):
            calls.append((url, headers, kw))
            return url.encode()

        server._set_job("job1")
        with mock.patch.object(server, "_fetch", side_effect=fake_fetch):
            files = server._download_segments(segs, self.tmp, {"Referer": "r"}, "job1")

        self.assertEqual([f.name for f in files], ["seg_00000.ts", "seg_00001.ts", "seg_00002.ts"])
        self.assertEqual(files[1].read_bytes(), b"https://a.com/s1.ts?x=1")
        job = server._jobs["job1"]
        self.assertEqual((job["total"], job["current"], job["stage"]), (3, 3, "downloading"))
        # Every segment shares one connection pool and gets the caller's headers.
        self.assertTrue(all(h == {"Referer": "r"} for _, h, _ in calls))
        pools = {id(kw["pool"]) for _, _, kw in calls}
        self.assertEqual(len(pools), 1)
        self.assertIsInstance(calls[0][2]["pool"], server._ConnectionPool)

    def test_empty_segment_list(self):
        server._set_job("job0")
        with mock.patch.object(server, "_fetch") as fetch:
            self.assertEqual(server._download_segments([], self.tmp, None, "job0"), [])
        fetch.assert_not_called()
        self.assertEqual(server._jobs["job0"]["total"], 0)

    def test_order_preserved_when_segments_complete_out_of_order(self):
        n = 5
        segs = self._segs(n)
        finished = [threading.Event() for _ in range(n)]
        completion_order = []
        lock = threading.Lock()

        def fake_fetch(url, headers=None, **kw):
            i = segs.index(url)
            # Segment i only completes after segment i+1: completion order is n-1, ..., 0.
            if i + 1 < n and not finished[i + 1].wait(self.WAIT):
                raise TimeoutError(f"segment {i + 1} never finished")
            with lock:
                completion_order.append(i)
            finished[i].set()
            return f"data{i}".encode()

        server._set_job("job1")
        with mock.patch.object(server, "_fetch", side_effect=fake_fetch):
            files = server._download_segments(segs, self.tmp, None, "job1")

        self.assertEqual(completion_order, list(reversed(range(n))))
        self.assertEqual([f.name for f in files], [f"seg_{i:05d}.ts" for i in range(n)])
        self.assertEqual([f.read_bytes() for f in files], [f"data{i}".encode() for i in range(n)])
        self.assertEqual(server._jobs["job1"]["current"], n)

    def test_downloads_run_in_parallel_up_to_the_worker_limit(self):
        server.SEGMENT_WORKERS = 3
        segs = self._segs(9)
        barrier = threading.Barrier(3, timeout=self.WAIT)  # needs 3 concurrent fetches to pass
        active = 0
        peak = 0
        lock = threading.Lock()

        def fake_fetch(url, headers=None, **kw):
            nonlocal active, peak
            with lock:
                active += 1
                peak = max(peak, active)
            try:
                barrier.wait()
                return b"x"
            finally:
                with lock:
                    active -= 1

        server._set_job("job1")
        with mock.patch.object(server, "_fetch", side_effect=fake_fetch):
            files = server._download_segments(segs, self.tmp, None, "job1")
        self.assertEqual(len(files), 9)
        self.assertEqual(peak, 3)

    def test_progress_counts_up_to_total_under_the_jobs_lock(self):
        n = 25
        segs = self._segs(n)
        seen = []
        real_set_job = server._set_job

        def recording_set_job(job_id, **kwargs):
            if "current" in kwargs:
                seen.append(kwargs["current"])
            real_set_job(job_id, **kwargs)

        lock_held = []
        real_lock = server._jobs_lock

        class CheckingLock:
            def __enter__(self):
                real_lock.acquire()
                lock_held.append(True)
                return self

            def __exit__(self, *exc):
                real_lock.release()

        def fake_fetch(url, headers=None, **kw):
            return b"x"

        server._set_job("job1")
        with mock.patch.object(server, "_fetch", side_effect=fake_fetch), \
                mock.patch.object(server, "_set_job", side_effect=recording_set_job), \
                mock.patch.object(server, "_jobs_lock", CheckingLock()):
            server._download_segments(segs, self.tmp, None, "job1")

        self.assertEqual(seen, list(range(n + 1)))  # 0, then one step per finished segment
        self.assertEqual(len(lock_held), n + 1)     # every update went through the lock
        job = server._jobs["job1"]
        self.assertEqual((job["current"], job["total"]), (n, n))

    def test_each_segment_gets_its_own_retries(self):
        segs = self._segs(4)
        attempts = {}
        pools_seen = {}
        lock = threading.Lock()

        def flaky_fetch_once(url, headers, pool=None):
            with lock:
                attempts[url] = attempts.get(url, 0) + 1
                pools_seen.setdefault(url, []).append(pool is not None)
                n = attempts[url]
            if n < server.MAX_RETRIES:
                raise ConnectionResetError(f"attempt {n}")
            return url.encode()

        server._set_job("job1")
        with mock.patch.object(server, "_fetch_once", side_effect=flaky_fetch_once):
            files = server._download_segments(segs, self.tmp, None, "job1")

        self.assertEqual(len(files), 4)
        self.assertEqual(attempts, {u: server.MAX_RETRIES for u in segs})
        # The first attempt uses the shared pool; after a transport error, urllib takes over.
        for url in segs:
            self.assertEqual(pools_seen[url], [True] + [False] * (server.MAX_RETRIES - 1))
        self.assertEqual(server._jobs["job1"]["current"], 4)

    def test_permanent_failure_returns_fewer_files(self):
        segs = self._segs(3)
        ok_done = threading.Event()

        def fake_fetch(url, headers=None, **kw):
            if url.endswith("s1.ts"):
                ok_done.wait(self.WAIT)  # let the other segments start first
                raise OSError("gone")
            if url.endswith("s2.ts"):
                ok_done.set()
            return b"x"

        server._set_job("job1")
        with mock.patch.object(server, "_fetch", side_effect=fake_fetch):
            files = server._download_segments(segs, self.tmp, None, "job1")
        self.assertEqual([f.name for f in files], ["seg_00000.ts", "seg_00002.ts"])
        self.assertEqual(server._jobs["job1"]["current"], 2)

    def test_permanent_failure_cancels_segments_that_have_not_started(self):
        server.SEGMENT_WORKERS = 1
        segs = self._segs(6)
        fetched = []

        def fake_fetch(url, headers=None, **kw):
            fetched.append(url)
            if url.endswith("s0.ts"):
                raise OSError("gone")
            # Give the main thread time to cancel the queued segments.
            threading.Event().wait(0.5)
            return b"x"

        server._set_job("job1")
        with mock.patch.object(server, "_fetch", side_effect=fake_fetch):
            files = server._download_segments(segs, self.tmp, None, "job1")
        # s0 failed; at most the segment already picked up by the worker ran.
        self.assertLessEqual(len(fetched), 2)
        self.assertEqual(fetched[0], segs[0])
        self.assertLess(len(files), len(segs))


class _LoopbackHandler(http.server.BaseHTTPRequestHandler):
    """HTTP/1.1 keep-alive test server on 127.0.0.1 (nothing leaves the machine)."""

    protocol_version = "HTTP/1.1"

    def setup(self):
        super().setup()
        with self.server.stats_lock:
            self.server.connections += 1

    def do_GET(self):
        with self.server.stats_lock:
            self.server.requests.append((self.path, dict(self.headers)))
        if self.path.startswith("/redirect-to-file"):
            self._send(302, b"", {"Location": "file:///C:/Windows/win.ini"})
        elif self.path.startswith("/redirect"):
            self._send(302, b"", {"Location": "/seg/redirected.ts"})
        elif self.path.startswith("/missing"):
            self._send(404, b"not found")
        else:
            self._send(200, self.path.encode())

    def _send(self, status, body, headers=None):
        self.send_response(status)
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class LoopbackHttpTests(_ServerStateMixin, unittest.TestCase):
    """Real http.client/urllib traffic against a local server, to prove keep-alive
    reuse and redirect handling end to end."""

    def setUp(self):
        super().setUp()
        httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _LoopbackHandler)
        httpd.daemon_threads = True
        httpd.block_on_close = False
        httpd.stats_lock = threading.Lock()
        httpd.connections = 0
        httpd.requests = []
        self.httpd = httpd
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        self.base = f"http://127.0.0.1:{httpd.server_address[1]}"
        # Never route loopback traffic through a system proxy.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), server._NoRedirectHandler)
        p = mock.patch.object(server, "_URLLIB_OPENER", opener)
        p.start()
        self.addCleanup(p.stop)

    def test_pool_reuses_one_connection_for_sequential_requests(self):
        with server._ConnectionPool(proxies={}) as pool:
            for i in range(5):
                self.assertEqual(server._fetch(f"{self.base}/seg/{i}.ts", {"Referer": "r"}, pool=pool),
                                 f"/seg/{i}.ts".encode())
        self.assertEqual(self.httpd.connections, 1)
        self.assertEqual(len(self.httpd.requests), 5)
        self.assertEqual(self.httpd.requests[0][1].get("Referer"), "r")
        self.assertEqual(self.httpd.requests[0][1].get("User-Agent"), server._DEFAULT_UA)

    def test_urllib_path_opens_a_connection_per_request(self):
        for i in range(3):
            server._fetch(f"{self.base}/seg/{i}.ts")
        self.assertEqual(self.httpd.connections, 3)

    def test_redirects_followed_on_both_transports(self):
        with server._ConnectionPool(proxies={}) as pool:
            self.assertEqual(server._fetch(f"{self.base}/redirect", pool=pool), b"/seg/redirected.ts")
        self.assertEqual(server._fetch(f"{self.base}/redirect"), b"/seg/redirected.ts")
        self.assertEqual([p for p, _ in self.httpd.requests],
                         ["/redirect", "/seg/redirected.ts"] * 2)

    def test_redirect_to_file_refused_on_both_transports(self):
        with server._ConnectionPool(proxies={}) as pool:
            with self.assertRaises(ValueError):
                server._fetch(f"{self.base}/redirect-to-file", pool=pool)
        with self.assertRaises(ValueError):
            server._fetch(f"{self.base}/redirect-to-file")
        self.assertEqual(len(self.httpd.requests), 2)  # no retries

    def test_http_error_status(self):
        with server._ConnectionPool(proxies={}) as pool:
            with self.assertRaises(urllib.error.HTTPError) as ctx:
                server._fetch(f"{self.base}/missing", retries=2, pool=pool)
        self.assertEqual(ctx.exception.code, 404)
        self.assertEqual(self.httpd.connections, 1)  # both attempts on the kept-alive connection

    def test_download_segments_end_to_end(self):
        server.SEGMENT_WORKERS = 3
        segs = [f"{self.base}/seg/{i}.ts" for i in range(12)]
        server._set_job("job1")
        with mock.patch.object(server.urllib.request, "getproxies", return_value={}):
            files = server._download_segments(segs, self.tmp, None, "job1")
        self.assertEqual([f.read_bytes() for f in files], [f"/seg/{i}.ts".encode() for i in range(12)])
        self.assertLessEqual(self.httpd.connections, 3)
        self.assertEqual(server._jobs["job1"]["current"], 12)


class MuxWithFfmpegTests(_ServerStateMixin, unittest.TestCase):
    def setUp(self):
        super().setUp()
        self.segs = []
        for i in range(2):
            p = self.tmp / f"seg_{i:05d}.ts"
            p.write_bytes(b"ts")
            self.segs.append(p)
        self.output = self.download_dir / "out.mp4"

    def test_success_builds_command_and_removes_concat_list(self):
        def fake_run(cmd, capture_output):
            concat = Path(cmd[cmd.index("-i") + 1])
            self.concat_text = concat.read_text(encoding="utf-8")
            Path(cmd[-1]).write_bytes(b"\0" * 2048)
            return mock.Mock(returncode=0, stderr=b"")

        with mock.patch.object(server.subprocess, "run", side_effect=fake_run) as run:
            server._mux_with_ffmpeg(self.segs, self.output, {"title": "T", "artist": "", "date": "2026"})

        cmd = run.call_args.args[0]
        self.assertEqual(cmd[0], server.FFMPEG_PATH)
        self.assertEqual(cmd[-1], str(self.output))
        self.assertIn("-metadata", cmd)
        self.assertIn("title=T", cmd)
        self.assertIn("date=2026", cmd)
        self.assertNotIn("artist=", cmd)
        self.assertIn(f"file '{self.segs[0].as_posix()}'", self.concat_text)
        self.assertFalse((self.download_dir / ".out_concat.txt").exists())

    def test_paths_with_apostrophes_are_escaped_for_the_concat_demuxer(self):
        seg = self.tmp / "O'Brien" / "seg_00000.ts"
        seg.parent.mkdir()
        seg.write_bytes(b"ts")

        def fake_run(cmd, capture_output):
            self.concat_text = Path(cmd[cmd.index("-i") + 1]).read_text(encoding="utf-8")
            Path(cmd[-1]).write_bytes(b"mp4")
            return mock.Mock(returncode=0, stderr=b"")

        with mock.patch.object(server.subprocess, "run", side_effect=fake_run):
            server._mux_with_ffmpeg([seg], self.output)
        escaped = seg.as_posix().replace("'", "'\\''")
        self.assertEqual(self.concat_text, f"file '{escaped}'\n")
        self.assertEqual(server._concat_quote("a'b"), "'a'\\''b'")

    def test_concat_list_is_removed_when_ffmpeg_fails(self):
        with mock.patch.object(server.subprocess, "run", return_value=mock.Mock(returncode=1, stderr=b"bad")):
            with self.assertRaises(RuntimeError):
                server._mux_with_ffmpeg(self.segs, self.output)
        self.assertFalse((self.download_dir / ".out_concat.txt").exists())

    def test_failure_raises_with_stderr_tail(self):
        with mock.patch.object(
            server.subprocess, "run",
            return_value=mock.Mock(returncode=1, stderr=b"Invalid data found"),
        ):
            with self.assertRaises(RuntimeError) as ctx:
                server._mux_with_ffmpeg(self.segs, self.output)
        self.assertIn("code 1", str(ctx.exception))
        self.assertIn("Invalid data found", str(ctx.exception))


class RunDownloadTests(_ServerStateMixin, unittest.TestCase):
    PLAYLIST_URL = "https://cdn.a.com/v/720p/index.m3u8"
    PLAYLIST = b"#EXTM3U\n#EXTINF:4,\ns0.ts\n#EXTINF:4,\ns1.ts\n#EXT-X-ENDLIST\n"

    def setUp(self):
        super().setUp()
        self.work = self.tmp / "work"
        self.work.mkdir()
        p = mock.patch.object(server.tempfile, "mkdtemp", return_value=str(self.work))
        p.start()
        self.addCleanup(p.stop)

    def _fake_fetch(self, url, headers=None, **kw):
        if url == self.PLAYLIST_URL:
            return self.PLAYLIST
        return b"segment"

    def test_success_pipeline(self):
        def fake_mux(seg_files, output_path, metadata=None):
            self.mux_args = (list(seg_files), output_path, metadata)
            output_path.write_bytes(b"mp4")

        server._set_job("job1")
        server.AUTO_OPEN = False
        with mock.patch.object(server, "_fetch", side_effect=self._fake_fetch), \
                mock.patch.object(server, "_mux_with_ffmpeg", side_effect=fake_mux), \
                mock.patch.object(server, "_open_file") as open_file:
            server._run_download("job1", self.PLAYLIST_URL, "My: Video", None, {"channel": "C"})

        job = server._jobs["job1"]
        self.assertEqual(job["status"], "done", job.get("error"))
        expected = self.download_dir / "My__Video_720P_job1.mp4"
        self.assertEqual(job["output"], str(expected))
        self.assertEqual((job["current"], job["total"]), (2, 2))
        self.assertIsNotNone(job["finished_at"])
        self.assertEqual(self.mux_args[1], expected)
        self.assertEqual(self.mux_args[2], {"title": "My: Video", "artist": "C"})
        self.assertIn(os.path.normcase(str(expected.resolve())), server._produced_outputs)
        self.assertFalse(self.work.exists(), "temp segment dir should be removed")
        open_file.assert_not_called()

    def test_auto_open(self):
        server.AUTO_OPEN = True
        with mock.patch.object(server, "_fetch", side_effect=self._fake_fetch), \
                mock.patch.object(server, "_mux_with_ffmpeg"), \
                mock.patch.object(server, "_open_file") as open_file:
            server._run_download("job2", self.PLAYLIST_URL, "", None, {})
        open_file.assert_called_once_with(self.download_dir / "720P_job2.mp4")

    def test_empty_playlist_marks_error(self):
        with mock.patch.object(server, "_fetch", return_value=b"#EXTM3U\n"), \
                mock.patch.object(server, "_mux_with_ffmpeg") as mux:
            server._run_download("job3", self.PLAYLIST_URL, "", None, {})
        job = server._jobs["job3"]
        self.assertEqual((job["status"], job["stage"]), ("error", "error"))
        self.assertIn("No .ts segments", job["error"])
        self.assertIsNotNone(job["finished_at"])
        mux.assert_not_called()

    def test_all_segments_failing_marks_error(self):
        def fetch(url, headers=None, **kw):
            if url == self.PLAYLIST_URL:
                return self.PLAYLIST
            raise OSError("404")

        with mock.patch.object(server, "_fetch", side_effect=fetch):
            server._run_download("job4", self.PLAYLIST_URL, "", None, {})
        self.assertIn("No segments could be downloaded", server._jobs["job4"]["error"])

    def test_missing_segments_fail_the_job_instead_of_saving_a_gappy_video(self):
        s0_done = threading.Event()

        def fetch(url, headers=None, **kw):
            if url == self.PLAYLIST_URL:
                return self.PLAYLIST
            if url.endswith("s1.ts"):
                s0_done.wait(5)
                raise OSError("timed out")
            s0_done.set()
            return b"ts"

        with mock.patch.object(server, "_fetch", side_effect=fetch), \
                mock.patch.object(server, "_mux_with_ffmpeg") as mux:
            server._run_download("job7", self.PLAYLIST_URL, "", None, {})
        job = server._jobs["job7"]
        self.assertEqual(job["status"], "error")
        self.assertIn("only 1 of 2 segments", job["error"])
        mux.assert_not_called()
        self.assertFalse(self.work.exists())

    def test_one_segment_failing_after_its_retries_fails_the_job(self):
        playlist = b"#EXTM3U\n" + b"".join(b"#EXTINF:4,\ns%d.ts\n" % i for i in range(8))
        attempts = {}
        lock = threading.Lock()

        def fetch_once(url, headers, pool=None):
            if url == self.PLAYLIST_URL:
                return playlist
            with lock:
                attempts[url] = attempts.get(url, 0) + 1
            if url.endswith("/s3.ts"):
                raise OSError("connection timed out")
            return b"ts"

        with mock.patch.object(server, "_fetch_once", side_effect=fetch_once), \
                mock.patch.object(server, "_mux_with_ffmpeg") as mux:
            server._run_download("job8", self.PLAYLIST_URL, "", None, {})
        job = server._jobs["job8"]
        self.assertEqual((job["status"], job["stage"]), ("error", "error"))
        self.assertIn("of 8 segments were downloaded", job["error"])
        self.assertEqual(attempts["https://cdn.a.com/v/720p/s3.ts"], server.MAX_RETRIES)
        mux.assert_not_called()
        self.assertFalse(self.work.exists())
        self.assertEqual(os.listdir(self.download_dir), [])

    def test_ffmpeg_failure_marks_error(self):
        with mock.patch.object(server, "_fetch", side_effect=self._fake_fetch), \
                mock.patch.object(server.subprocess, "run",
                                  return_value=mock.Mock(returncode=1, stderr=b"bad")):
            server._run_download("job5", self.PLAYLIST_URL, "", None, {})
        job = server._jobs["job5"]
        self.assertEqual(job["status"], "error")
        self.assertIn("ffmpeg failed", job["error"])
        self.assertEqual(server._produced_outputs, set())

    def test_temp_dir_removed_on_failure(self):
        with mock.patch.object(server, "_fetch", side_effect=self._fake_fetch), \
                mock.patch.object(server, "_mux_with_ffmpeg", side_effect=RuntimeError("ffmpeg failed")):
            server._run_download("job6", self.PLAYLIST_URL, "", None, {})
        self.assertEqual(server._jobs["job6"]["status"], "error")
        self.assertFalse(self.work.exists())


# ── HTTP endpoints ──────────────────────────────────────────────────────────

class _ClientMixin(_ServerStateMixin):
    def setUp(self):
        super().setUp()
        server.SERVER_HOST = "127.0.0.1"
        self.client = server.app.test_client()
        p = mock.patch.object(server, "_open_file")
        self.open_file = p.start()
        self.addCleanup(p.stop)
        # Safety net: no endpoint test may spawn a real download thread.
        t = mock.patch.object(server.threading, "Thread")
        self.thread_cls = t.start()
        self.addCleanup(t.stop)


class HealthEndpointTests(_ClientMixin, unittest.TestCase):
    def test_health(self):
        r = self.client.get("/health")
        self.assertEqual(r.status_code, 200)
        self.assertEqual(
            r.get_json(),
            {"ok": True, "ffmpeg": server.FFMPEG_PATH, "download_dir": str(self.download_dir)},
        )


class OriginGuardTests(_ClientMixin, unittest.TestCase):
    def test_no_origin_allowed(self):
        self.assertEqual(self.client.get("/health").status_code, 200)

    def test_extension_origins_allowed(self):
        for origin in (EXT_ORIGIN, MOZ_ORIGIN):
            with self.subTest(origin=origin):
                r = self.client.get("/health", headers={"Origin": origin})
                self.assertEqual(r.status_code, 200)

    def test_web_origins_rejected(self):
        for origin in (
            "https://evil.example.com",
            "http://localhost:3000",
            "http://127.0.0.1:8765",
            "null",
            "",
            "file://",
            "chrome-extension://",
            "chrome-extension://abc/path",
            "chrome-extension://abc.evil.com:443/",
            "https://chrome-extension://abc",
            "safari-web-extension://abc",
            "CHROME-EXTENSION://abc",
        ):
            with self.subTest(origin=origin):
                r = self.client.get("/health", headers={"Origin": origin})
                self.assertEqual(r.status_code, 403)
                self.assertEqual(r.get_json(), {"error": "Forbidden origin"})
                self.assertNotIn("Access-Control-Allow-Origin", r.headers)

    def test_web_origin_cannot_open_files(self):
        f = self.download_dir / "video.mp4"
        f.write_bytes(b"x")
        r = self.client.post("/open", json={"path": str(f)}, headers={"Origin": "https://evil.com"})
        self.assertEqual(r.status_code, 403)
        self.open_file.assert_not_called()

    def test_web_origin_cannot_start_download(self):
        r = self.client.post(
            "/download", json={"url": "https://a.com/x.m3u8"}, headers={"Origin": "https://evil.com"}
        )
        self.assertEqual(r.status_code, 403)
        self.thread_cls.assert_not_called()
        self.assertEqual(server._jobs, {})

    def test_web_origin_preflight_rejected(self):
        r = self.client.options(
            "/open",
            headers={"Origin": "https://evil.com", "Access-Control-Request-Method": "POST"},
        )
        self.assertEqual(r.status_code, 403)
        self.assertNotIn("Access-Control-Allow-Origin", r.headers)


class CorsTests(_ClientMixin, unittest.TestCase):
    def test_echoes_extension_origin(self):
        for origin in (EXT_ORIGIN, MOZ_ORIGIN):
            with self.subTest(origin=origin):
                r = self.client.get("/health", headers={"Origin": origin})
                self.assertEqual(r.headers.get("Access-Control-Allow-Origin"), origin)

    def test_preflight_from_extension(self):
        r = self.client.options(
            "/download",
            headers={
                "Origin": EXT_ORIGIN,
                "Access-Control-Request-Method": "POST",
                "Access-Control-Request-Headers": "Content-Type",
            },
        )
        self.assertLess(r.status_code, 400)
        self.assertEqual(r.headers.get("Access-Control-Allow-Origin"), EXT_ORIGIN)
        self.assertIn("POST", r.headers.get("Access-Control-Allow-Methods", ""))

    def test_no_cors_header_without_origin(self):
        r = self.client.get("/health")
        self.assertNotIn("Access-Control-Allow-Origin", r.headers)


class HostGuardTests(_ClientMixin, unittest.TestCase):
    def _get(self, host, **kw):
        return self.client.get("/health", headers={"Host": host}, **kw)

    def test_loopback_hosts_allowed(self):
        for host in ("127.0.0.1", "127.0.0.1:8765", "localhost", "localhost:8765",
                     "LOCALHOST:8765", "[::1]:8765"):
            with self.subTest(host=host):
                self.assertEqual(self._get(host).status_code, 200)

    def test_non_loopback_hosts_rejected(self):
        for host in ("evil.com", "evil.com:8765", "192.168.1.10:8765", "localhost.evil.com",
                     "127.0.0.1.nip.io:8765", "0.0.0.0:8765"):
            with self.subTest(host=host):
                r = self._get(host)
                self.assertEqual(r.status_code, 403)
                self.assertEqual(r.get_json(), {"error": "Forbidden host"})

    def test_rebinding_with_extension_origin_still_rejected(self):
        r = self.client.get("/health", headers={"Host": "attacker.com:8765", "Origin": EXT_ORIGIN})
        self.assertEqual(r.status_code, 403)

    def test_host_check_skipped_when_not_bound_to_loopback(self):
        server.SERVER_HOST = "0.0.0.0"
        self.assertEqual(self._get("192.168.1.10:8765").status_code, 200)

    def test_host_check_active_for_ipv6_and_localhost_binds(self):
        for bind in ("::1", "localhost"):
            server.SERVER_HOST = bind
            with self.subTest(bind=bind):
                self.assertEqual(self._get("evil.com").status_code, 403)

    def test_ipv6_loopback_without_port_allowed(self):
        self.assertEqual(self._get("[::1]").status_code, 200)


class DownloadEndpointTests(_ClientMixin, unittest.TestCase):
    def test_missing_url(self):
        for body in ({}, {"url": ""}, {"url": None}, {"title": "x"}):
            with self.subTest(body=body):
                r = self.client.post("/download", json=body)
                self.assertEqual(r.status_code, 400)
                self.assertEqual(r.get_json(), {"error": "Missing 'url' field"})
        self.thread_cls.assert_not_called()

    def test_rejects_non_http_urls(self):
        for url in ("file:///C:/Windows/win.ini", "ftp://a.com/x.m3u8", "javascript:alert(1)",
                    "C:\\video.m3u8", "\\\\server\\share\\x.m3u8", 12345, ["https://a.com/x"],
                    {"u": "https://a"}):
            with self.subTest(url=url):
                r = self.client.post("/download", json={"url": url})
                self.assertEqual(r.status_code, 400)
                self.assertEqual(r.get_json(), {"error": "Only http(s) URLs are supported"})
        self.thread_cls.assert_not_called()
        self.assertEqual(server._jobs, {})

    def test_starts_background_job(self):
        body = {
            "url": "https://cdn.a.com/v/index.m3u8",
            "title": "Hello",
            "headers": {"Referer": "https://a.com/"},
            "pageUrl": "https://a.com/watch",
            "metadata": {"channel": "Chan"},
        }
        r = self.client.post("/download", json=body, headers={"Origin": EXT_ORIGIN})
        self.assertEqual(r.status_code, 200)
        payload = r.get_json()
        self.assertTrue(payload["ok"])
        job_id = payload["job_id"]
        self.assertRegex(job_id, r"^[0-9a-f]{8}$")
        self.assertEqual(r.headers.get("Access-Control-Allow-Origin"), EXT_ORIGIN)

        self.thread_cls.assert_called_once()
        kwargs = self.thread_cls.call_args.kwargs
        self.assertIs(kwargs["target"], server._run_download)
        self.assertTrue(kwargs["daemon"])
        self.assertEqual(
            kwargs["args"],
            (job_id, body["url"], "Hello", {"Referer": "https://a.com/"},
             {"title": "Hello", "pageUrl": "https://a.com/watch", "channel": "Chan"}),
        )
        self.thread_cls.return_value.start.assert_called_once_with()
        self.assertEqual(server._jobs[job_id]["status"], "running")

    def test_force_json_without_content_type(self):
        r = self.client.post("/download", data='{"url": "http://a.com/x.m3u8"}',
                             content_type="text/plain")
        self.assertEqual(r.status_code, 200)
        self.thread_cls.assert_called_once()

    def test_invalid_json_is_400(self):
        r = self.client.post("/download", data="not json", content_type="application/json")
        self.assertEqual(r.status_code, 400)
        self.thread_cls.assert_not_called()

    def test_opportunistic_cleanup_of_old_jobs(self):
        server._set_job("stale", status="done", finished_at=time() - server.JOB_TTL_SECONDS - 1)
        r = self.client.post("/download", json={"url": "https://a.com/x.m3u8"})
        self.assertEqual(r.status_code, 200)
        self.assertNotIn("stale", server._jobs)
        self.assertIn(r.get_json()["job_id"], server._jobs)

    def test_non_object_json_body_is_400(self):
        r = self.client.post("/download", json=["https://a.com/x.m3u8"])
        self.assertEqual(r.status_code, 400)


class ProgressEndpointTests(_ClientMixin, unittest.TestCase):
    def test_unknown_job(self):
        r = self.client.get("/progress/nope")
        self.assertEqual(r.status_code, 404)
        self.assertEqual(r.get_json(), {"error": "Unknown job"})

    def test_known_job(self):
        server._set_job("abc12345", total=10, current=3, stage="downloading")
        r = self.client.get("/progress/abc12345", headers={"Origin": EXT_ORIGIN})
        self.assertEqual(r.status_code, 200)
        job = r.get_json()
        self.assertEqual((job["status"], job["total"], job["current"], job["stage"]),
                         ("running", 10, 3, "downloading"))

    def test_job_from_download_endpoint(self):
        job_id = self.client.post("/download", json={"url": "https://a.com/x.m3u8"}).get_json()["job_id"]
        r = self.client.get(f"/progress/{job_id}")
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.get_json()["stage"], "starting")


class LogsEndpointTests(_ClientMixin, unittest.TestCase):
    def setUp(self):
        super().setUp()
        server.LOG_FILE = self.tmp / "test.log"

    def _write(self, n):
        server.LOG_FILE.write_text("".join(f"line {i}\n" for i in range(n)), encoding="utf-8")

    def test_server_log_is_rotated(self):
        from logging.handlers import RotatingFileHandler
        rotating = [h for h in _IMPORT_LOG_FILE_HANDLERS if isinstance(h, RotatingFileHandler)]
        self.assertEqual(len(rotating), 1)
        self.assertGreater(rotating[0].maxBytes, 0)
        self.assertGreater(rotating[0].backupCount, 0)

    def test_missing_log_file(self):
        r = self.client.get("/logs")
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.get_json(), {"log": ""})

    def test_default_returns_last_200(self):
        self._write(250)
        lines = self.client.get("/logs").get_json()["log"].splitlines()
        self.assertEqual(len(lines), 200)
        self.assertEqual(lines[0], "line 50")
        self.assertEqual(lines[-1], "line 249")

    def test_explicit_count(self):
        self._write(10)
        self.assertEqual(self.client.get("/logs?lines=3").get_json()["log"], "line 7\nline 8\nline 9\n")

    def test_clamped_to_minimum_one(self):
        self._write(10)
        for v in ("0", "-5"):
            with self.subTest(lines=v):
                self.assertEqual(self.client.get(f"/logs?lines={v}").get_json()["log"], "line 9\n")

    def test_clamped_to_maximum_2000(self):
        self._write(2500)
        lines = self.client.get("/logs?lines=999999").get_json()["log"].splitlines()
        self.assertEqual(len(lines), 2000)
        self.assertEqual(lines[0], "line 500")

    def test_invalid_count_falls_back_to_200(self):
        self._write(300)
        for v in ("abc", "1.5", ""):
            with self.subTest(lines=v):
                lines = self.client.get(f"/logs?lines={v}").get_json()["log"].splitlines()
                self.assertEqual(len(lines), 200)

    def test_invalid_utf8_replaced(self):
        server.LOG_FILE.write_bytes(b"ok\n\xff\xfe bad\n")
        r = self.client.get("/logs")
        self.assertEqual(r.status_code, 200)
        self.assertIn("ok", r.get_json()["log"])


class OpenEndpointTests(_ClientMixin, unittest.TestCase):
    def test_missing_path(self):
        for body in ({}, {"path": ""}, {"path": None}):
            with self.subTest(body=body):
                r = self.client.post("/open", json=body)
                self.assertEqual(r.status_code, 400)
        self.open_file.assert_not_called()

    def test_opens_file_in_download_dir(self):
        f = self.download_dir / "video.mp4"
        f.write_bytes(b"x")
        r = self.client.post("/open", json={"path": str(f)}, headers={"Origin": EXT_ORIGIN})
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.get_json(), {"ok": True})
        self.open_file.assert_called_once_with(f.resolve())

    def test_opens_file_in_download_subdir(self):
        f = self.download_dir / "a" / "b" / "video.mp4"
        f.parent.mkdir(parents=True)
        f.write_bytes(b"x")
        self.assertEqual(self.client.post("/open", json={"path": str(f)}).status_code, 200)

    def test_missing_file_404(self):
        r = self.client.post("/open", json={"path": str(self.download_dir / "nope.mp4")})
        self.assertEqual(r.status_code, 404)
        self.assertEqual(r.get_json(), {"error": "File not found"})
        self.open_file.assert_not_called()

    def test_directory_is_404(self):
        r = self.client.post("/open", json={"path": str(self.download_dir)})
        self.assertEqual(r.status_code, 404)
        self.open_file.assert_not_called()

    def test_file_outside_download_dir_403(self):
        f = self.tmp / "calc.exe"
        f.write_bytes(b"MZ")
        r = self.client.post("/open", json={"path": str(f)})
        self.assertEqual(r.status_code, 403)
        self.assertEqual(r.get_json(), {"error": "Only downloaded files can be opened"})
        self.open_file.assert_not_called()

    def test_system_executable_403(self):
        exe = Path(sys.executable)
        r = self.client.post("/open", json={"path": str(exe)})
        self.assertEqual(r.status_code, 403)
        self.open_file.assert_not_called()

    def test_path_traversal_out_of_download_dir_403(self):
        f = self.tmp / "evil.bat"
        f.write_text("echo pwned")
        traversal = str(self.download_dir / ".." / "evil.bat")
        r = self.client.post("/open", json={"path": traversal})
        self.assertEqual(r.status_code, 403)
        self.open_file.assert_not_called()

    def test_relative_path_resolved_against_cwd_not_download_dir(self):
        # A relative path resolves against the server's CWD; it must not be
        # allowed just because it looks like a bare filename.
        f = self.tmp / "rel.txt"
        f.write_text("x")
        old = os.getcwd()
        os.chdir(self.tmp)
        try:
            r = self.client.post("/open", json={"path": "rel.txt"})
        finally:
            os.chdir(old)
        self.assertEqual(r.status_code, 403)
        self.open_file.assert_not_called()

    def test_produced_output_outside_current_download_dir_allowed(self):
        f = self.tmp / "previous_dir" / "video.mp4"
        f.parent.mkdir()
        f.write_bytes(b"x")
        server._produced_outputs.add(os.path.normcase(str(f.resolve())))
        r = self.client.post("/open", json={"path": str(f)})
        self.assertEqual(r.status_code, 200)
        self.open_file.assert_called_once_with(f.resolve())

    def test_file_allowed_after_download_dir_changes_only_if_produced(self):
        f = self.download_dir / "video.mp4"
        f.write_bytes(b"x")
        new_dir = self.tmp / "new_downloads"
        r = self.client.post("/config/download_dir", json={"path": str(new_dir)},
                             headers={"X-VSD-Admin-Token": ADMIN_TOKEN})
        self.assertEqual(r.status_code, 200)
        self.assertEqual(self.client.post("/open", json={"path": str(f)}).status_code, 403)
        server._produced_outputs.add(os.path.normcase(str(f.resolve())))
        self.assertEqual(self.client.post("/open", json={"path": str(f)}).status_code, 200)


class ConfigDownloadDirTests(_ClientMixin, unittest.TestCase):
    """POST requires X-VSD-Admin-Token == VSD_ADMIN_TOKEN. The token is the only
    authority: no Origin (MULA) or an allowed extension Origin are both accepted
    with it; web origins are rejected by the global Origin guard regardless."""

    TOKEN_HEADER = {"X-VSD-Admin-Token": ADMIN_TOKEN}

    def _post(self, path, headers=None):
        return self.client.post("/config/download_dir", json={"path": str(path)}, headers=headers or {})

    def _assert_refused(self, r, new_dir):
        self.assertEqual(r.status_code, 403)
        self.assertIn("only be changed by MULA", r.get_json()["error"])
        self.assertFalse(new_dir.exists())
        self.assertEqual(server.DOWNLOAD_DIR, self.download_dir)

    def test_token_read_from_env_and_removed_from_it(self):
        self.assertEqual(server.ADMIN_TOKEN, ADMIN_TOKEN)
        self.assertNotIn("VSD_ADMIN_TOKEN", os.environ)  # not inherited by ffmpeg / opened apps

    def test_get_needs_no_token(self):
        for headers in ({}, {"Origin": EXT_ORIGIN}):
            with self.subTest(headers=headers):
                r = self.client.get("/config/download_dir", headers=headers)
                self.assertEqual(r.status_code, 200)
                self.assertEqual(r.get_json(), {"download_dir": str(self.download_dir)})

    def test_post_with_correct_token_changes_dir_and_creates_it(self):
        new_dir = self.tmp / "nested" / "new"
        r = self._post(new_dir, self.TOKEN_HEADER)
        self.assertEqual(r.status_code, 200)
        self.assertEqual(r.get_json(), {"ok": True, "download_dir": str(new_dir)})
        self.assertTrue(new_dir.is_dir())
        self.assertEqual(server.DOWNLOAD_DIR, new_dir)
        self.assertEqual(self.client.get("/health").get_json()["download_dir"], str(new_dir))

    def test_post_missing_token_refused(self):
        self._assert_refused(self._post(self.tmp / "d"), self.tmp / "d")

    def test_post_wrong_token_refused(self):
        new_dir = self.tmp / "d"
        for token in ("wrong", ADMIN_TOKEN[:-1], ADMIN_TOKEN + "x", ADMIN_TOKEN.upper(),
                      " " + ADMIN_TOKEN, "", "Ω"):
            with self.subTest(token=token):
                self._assert_refused(self._post(new_dir, {"X-VSD-Admin-Token": token}), new_dir)

    def test_token_in_another_header_refused(self):
        new_dir = self.tmp / "d"
        for name in ("Authorization", "X-Admin-Token", "Cookie"):
            with self.subTest(header=name):
                self._assert_refused(self._post(new_dir, {name: ADMIN_TOKEN}), new_dir)

    def test_header_name_is_case_insensitive(self):
        new_dir = self.tmp / "d"
        self.assertEqual(self._post(new_dir, {"x-vsd-admin-token": ADMIN_TOKEN}).status_code, 200)

    def test_post_refused_when_token_not_configured(self):
        server.ADMIN_TOKEN = ""
        new_dir = self.tmp / "d"
        for headers in ({}, {"X-VSD-Admin-Token": ""}, {"X-VSD-Admin-Token": ADMIN_TOKEN}):
            with self.subTest(headers=headers):
                self._assert_refused(self._post(new_dir, headers), new_dir)

    def test_post_token_compared_in_constant_time(self):
        with mock.patch.object(server.hmac, "compare_digest", wraps=server.hmac.compare_digest) as cmp:
            self.assertEqual(self._post(self.tmp / "d", self.TOKEN_HEADER).status_code, 200)
        cmp.assert_called_once_with(ADMIN_TOKEN.encode(), ADMIN_TOKEN.encode())

    def test_post_missing_path(self):
        r = self.client.post("/config/download_dir", json={}, headers=self.TOKEN_HEADER)
        self.assertEqual(r.status_code, 400)
        self.assertEqual(server.DOWNLOAD_DIR, self.download_dir)

    def test_post_without_token_checked_before_body(self):
        r = self.client.post("/config/download_dir", data="not json", content_type="application/json")
        self.assertEqual(r.status_code, 403)

    def test_post_from_extension_origin_without_token_refused(self):
        new_dir = self.tmp / "ext_dir"
        for origin in (EXT_ORIGIN, MOZ_ORIGIN):
            with self.subTest(origin=origin):
                self._assert_refused(self._post(new_dir, {"Origin": origin}), new_dir)

    def test_post_from_extension_origin_with_token_allowed(self):
        # Documented decision: the token alone authorizes. Extensions are never given it.
        new_dir = self.tmp / "ext_dir"
        r = self._post(new_dir, {"Origin": EXT_ORIGIN, **self.TOKEN_HEADER})
        self.assertEqual(r.status_code, 200)
        self.assertEqual(server.DOWNLOAD_DIR, new_dir)

    def test_post_from_web_origin_refused_even_with_token(self):
        new_dir = self.tmp / "web_dir"
        r = self._post(new_dir, {"Origin": "https://evil.com", **self.TOKEN_HEADER})
        self.assertEqual(r.status_code, 403)
        self.assertEqual(r.get_json(), {"error": "Forbidden origin"})
        self.assertFalse(new_dir.exists())
        self.assertEqual(server.DOWNLOAD_DIR, self.download_dir)

    def test_post_with_token_but_rebinding_host_refused(self):
        new_dir = self.tmp / "d"
        r = self._post(new_dir, {"Host": "attacker.com:8765", **self.TOKEN_HEADER})
        self.assertEqual(r.status_code, 403)
        self.assertEqual(r.get_json(), {"error": "Forbidden host"})
        self.assertFalse(new_dir.exists())


if __name__ == "__main__":
    unittest.main()
