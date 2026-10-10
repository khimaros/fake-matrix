"""python client and process manager for the fake-matrix homeserver binary.

mirrors fakeopenai.py: spawn the real binary, point a bot's homeserver at base_url,
inject inbound traffic, and inspect captured sends over the http admin plane. the
tests run as plain python3, so this module is stdlib-only.

    import fakematrix
    if not fakematrix.available():
        print("SKIP: build ../fake-matrix"); sys.exit(0)
    with fakematrix.FakeMatrix() as fake:
        # point a bridge at fake.base_url (BRIDGE_HOMESERVER), drive it, then:
        fake.inject(fake.DM_ROOM, fake.HUMAN, "hey per")
        replies = fake.wait_sent(1)
        assert "per" in replies[0]["body"].lower()

module-level functions keyed on an admin url exist too, for a test that only holds
the url: fakematrix.sent(admin_url), fakematrix.inject(admin_url, ...), etc.
"""

import json
import os
import subprocess
import time
import urllib.request
from pathlib import Path

# the binary lives in this checkout (clients/python/ -> repo root is parents[2]).
# FAKE_MATRIX_BIN overrides, e.g. to point at a release build.
_REPO_ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("FAKE_MATRIX_BIN") or _REPO_ROOT / "target" / "debug" / "fake-matrix")

# the binary announces its chosen url on this stdout line; the admin plane shares
# the port under /_fake (a homeserver base url has no /v1 suffix to strip).
BASE_URL_PREFIX = "base_url="
STARTUP_TIMEOUT_S = 10
ADMIN_TIMEOUT_S = 5

# defaults that match src/config.rs, so a test can name the seeded rooms/users.
BOT_USER = "@per:fake.local"
# the bot's profile display name, served at /profile/<bot>/displayname (matches src/config.rs).
BOT_DISPLAY_NAME = "Persona"
HUMAN = "@tester:fake.local"
OTHER = "@other:fake.local"
DM_ROOM = "!dm:fake.local"
GROUP_ROOM = "!room:fake.local"
ENC_ROOM = "!enc:fake.local"
# a dm with OTHER that no test injects into: the room an agent-initiated message goes to.
QUIET_ROOM = "!quiet:fake.local"


def available():
    """true when the binary is built; tests skip themselves when it is not."""
    return BIN.exists()


def _admin(admin_url, path, method="GET", payload=None):
    """one admin request; returns the parsed json body (or {} when empty)."""
    data = json.dumps(payload).encode() if payload is not None else None
    headers = {"Content-Type": "application/json"} if data is not None else {}
    req = urllib.request.Request(admin_url + path, data=data, method=method, headers=headers)
    with urllib.request.urlopen(req, timeout=ADMIN_TIMEOUT_S) as r:
        raw = r.read().decode()
    return json.loads(raw) if raw else {}


# --- admin api keyed on an admin url ---

def sent(admin_url):
    """the bot's captured outbound sends, each {room_id, type, msgtype, body, content}."""
    return _admin(admin_url, "/sent").get("sent", [])


def media(filename, mimetype, data, msgtype=None):
    """the `media` of an inject: a file of `data` bytes. an image/* is sent as m.image, anything
    else as m.file, unless `msgtype` says otherwise."""
    kind = msgtype or ("m.image" if mimetype.startswith("image/") else "m.file")
    return {"msgtype": kind, "filename": filename, "mimetype": mimetype, "hex": data.hex()}


def inject(admin_url, room_id, sender, body, formatted_body=None, mentions=None, media=None):
    """queue an inbound message into a room's timeline; returns {ok, event_id}. m.text, or a file
    when `media` (see `media()`) is given: `body` is then its caption, or its filename for none.

    the event_id is what a REPLY to this message points at, so a test asserting that a bot
    answered a PARTICULAR message reads it from here rather than guessing the server's id format.

    formatted_body + mentions are what make an injection a real MENTION: the plain body renders a
    mention pill as its text alone ("Persona how are you?"), so which user was addressed survives
    only in the html anchor and in m.mentions. pass mentions as the m.mentions content itself,
    e.g. {"user_ids": [BOT_USER]}."""
    payload = {"room_id": room_id, "sender": sender, "body": body}
    if formatted_body is not None:
        payload["formatted_body"] = formatted_body
    if mentions is not None:
        payload["mentions"] = mentions
    if media is not None:
        payload["media"] = media
    return _admin(admin_url, "/inject", "POST", payload)


def mention(user_id, text=None):
    """the (formatted_body, mentions) pair for a message that opens by mentioning `user_id`, as a
    client sends it: an anchor pill whose text is the display name, plus the m.mentions list."""
    label = text or user_id
    return (f'<a href="https://matrix.to/#/{user_id}">{label}</a>', {"user_ids": [user_id]})


def receipts(admin_url):
    """read receipts clients posted, each {room_id, user_id, type, event_id}. a bot marks a
    message read once it has PROCESSED it, so this is how a test sees the ones it did not reply to."""
    return _admin(admin_url, "/receipts").get("receipts", [])


def wait_receipts(admin_url, count, timeout=30):
    """poll until at least `count` receipts are captured."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        r = receipts(admin_url)
        if len(r) >= count:
            return r
        time.sleep(0.1)
    return receipts(admin_url)


def rooms(admin_url):
    """the seeded rooms."""
    return _admin(admin_url, "/rooms").get("rooms", [])


def syncs(admin_url):
    """syncs served per user. wait for a client's count to rise before injecting: an initial
    sync returns state rather than backlog AND hands back a token past anything already in the
    log, so an event injected before it is never delivered to that client."""
    return _admin(admin_url, "/syncs").get("syncs", {})


def wait_synced(admin_url, user, timeout=30):
    """block until `user` has completed at least one sync. returns True if it did."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if syncs(admin_url).get(user, 0) >= 1:
            return True
        time.sleep(0.05)
    return False


def reset(admin_url):
    """clear the timeline and captured sends."""
    _admin(admin_url, "/reset", "POST", {})


def health(admin_url):
    return _admin(admin_url, "/health")


def wait_sent(admin_url, count, timeout=30):
    """poll until at least `count` sends are captured (the bot reply is asynchronous)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = sent(admin_url)
        if len(s) >= count:
            return s
        time.sleep(0.1)
    return sent(admin_url)


class FakeMatrix:
    """a running fake-matrix subprocess plus its admin api. use as a context manager
    so the process is always terminated. admin methods delegate to the module-level
    functions against self.admin_url."""

    BOT_USER = BOT_USER
    BOT_DISPLAY_NAME = BOT_DISPLAY_NAME
    HUMAN = HUMAN
    OTHER = OTHER
    DM_ROOM = DM_ROOM
    GROUP_ROOM = GROUP_ROOM
    ENC_ROOM = ENC_ROOM
    QUIET_ROOM = QUIET_ROOM

    def __init__(self, *args, timeout=STARTUP_TIMEOUT_S, log_path=None):
        self.args = args
        self.timeout = timeout
        self.proc = None
        self.base_url = None
        self.admin_url = None
        # where the server's stderr goes. unset -> discarded, as before. pass a path to KEEP it:
        # when a client never receives an injected event, the client log shows only silence, and
        # the useful half of the answer (did the server deliver it? to which members?) lives here.
        self.log_path = log_path
        self._log = None

    def start(self):
        """launch on a free port and block until base_url is announced."""
        self._log = open(self.log_path, "w") if self.log_path else subprocess.DEVNULL
        self.proc = subprocess.Popen(
            [str(BIN), "--port", "0", *self.args],
            stdout=subprocess.PIPE, stderr=self._log, text=True)
        deadline = time.time() + self.timeout
        while time.time() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                break
            if line.startswith(BASE_URL_PREFIX):
                url = line.strip()[len(BASE_URL_PREFIX):]
                self.base_url = url
                # 0.0.0.0 is a bind wildcard, not dialable; reach our own admin on loopback.
                admin_base = url.replace("://0.0.0.0:", "://127.0.0.1:")
                self.admin_url = admin_base + "/_fake"
                return self
        self.stop()
        raise RuntimeError("fake-matrix did not announce base_url")

    def stop(self):
        """terminate the subprocess if still running (idempotent)."""
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        if self._log not in (None, subprocess.DEVNULL):
            self._log.close()
            self._log = None

    def log(self):
        """the server's captured stderr, or a note when it was not captured. safe to call after
        stop(); a failing test should print this next to the client-side logs."""
        if not self.log_path:
            return "(fake-matrix stderr not captured; pass log_path= to keep it)"
        try:
            with open(self.log_path) as f:
                return f.read()
        except OSError as e:
            return f"(could not read {self.log_path}: {e})"

    def __enter__(self):
        return self.start()

    def __exit__(self, *exc):
        self.stop()
        return False

    def sent(self):
        return sent(self.admin_url)

    def inject(self, room_id, sender, body, formatted_body=None, mentions=None, media=None):
        return inject(self.admin_url, room_id, sender, body, formatted_body, mentions, media)

    def receipts(self):
        return receipts(self.admin_url)

    def wait_receipts(self, count, timeout=30):
        return wait_receipts(self.admin_url, count, timeout)

    def rooms(self):
        return rooms(self.admin_url)

    def syncs(self):
        return syncs(self.admin_url)

    def wait_synced(self, user=None, timeout=30):
        return wait_synced(self.admin_url, user or self.BOT_USER, timeout)

    def reset(self):
        reset(self.admin_url)

    def health(self):
        return health(self.admin_url)

    def wait_sent(self, count, timeout=30):
        return wait_sent(self.admin_url, count, timeout)
