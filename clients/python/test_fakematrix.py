#!/usr/bin/env python3
"""self-test for the fake-matrix binary + python client. drives the matrix
client-server endpoints directly over http (standing in for a bot), so it needs no
matrix-sdk. plain python3 with a PASS/FAIL counter, and self-skips when the binary
is not built -- matching the fake-openai / hmux e2e convention."""

import json
import sys
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fakematrix

PASS = 0
FAIL = 0


def check(desc, ok):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  ok   {desc}")
    else:
        FAIL += 1
        print(f"  FAIL {desc}")


def http(url, method="GET", payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=5) as r:
        raw = r.read().decode()
    return json.loads(raw) if raw else {}


def main():
    if not fakematrix.available():
        print(f"SKIP: build ../fake-matrix ({fakematrix.BIN})")
        return 0

    with fakematrix.FakeMatrix() as fake:
        base = fake.base_url

        versions = http(base + "/_matrix/client/versions")
        check("versions advertises v1.11", "v1.11" in versions.get("versions", []))

        who = http(base + "/_matrix/client/v3/account/whoami")
        check("whoami user_id", who.get("user_id") == fakematrix.BOT_USER)
        check("whoami device_id present", bool(who.get("device_id")))

        initial = http(base + "/_matrix/client/v3/sync")
        joined = initial.get("rooms", {}).get("join", {})
        check("initial sync joins the DM room", fakematrix.DM_ROOM in joined)
        check("initial sync joins the group room", fakematrix.GROUP_ROOM in joined)
        direct = [e for e in initial.get("account_data", {}).get("events", [])
                  if e.get("type") == "m.direct"]
        check("initial sync carries m.direct", bool(direct))
        def room_has_enc(rid):
            return any(ev.get("type") == "m.room.encryption"
                       for ev in joined.get(rid, {}).get("state", {}).get("events", []))
        check("the encrypted room advertises m.room.encryption", room_has_enc(fakematrix.ENC_ROOM))
        check("plaintext rooms do not advertise encryption",
              not room_has_enc(fakematrix.DM_ROOM) and not room_has_enc(fakematrix.GROUP_ROOM))
        since = initial["next_batch"]

        fake.inject(fakematrix.DM_ROOM, fakematrix.HUMAN, "hey per")
        incr = http(base + "/_matrix/client/v3/sync?since=" + since)
        timeline = (incr.get("rooms", {}).get("join", {})
                    .get(fakematrix.DM_ROOM, {}).get("timeline", {}).get("events", []))
        bodies = [e.get("content", {}).get("body") for e in timeline]
        check("injected message appears in incremental sync", "hey per" in bodies)
        fresh = timeline and timeline[0].get("sender") == fakematrix.HUMAN
        check("injected event sender is the human", bool(fresh))

        room_path = urllib.parse.quote(fakematrix.DM_ROOM, safe="")
        http(base + f"/_matrix/client/v3/rooms/{room_path}/send/m.room.message/txn1",
             "PUT", {"msgtype": "m.text", "body": "hello back"})
        captured = fake.sent()
        check("outbound send is captured", any(c.get("body") == "hello back" for c in captured))

        members = http(base + f"/_matrix/client/v3/rooms/{room_path}/members")
        check("members lists the bot", any(
            m.get("state_key") == fakematrix.BOT_USER for m in members.get("chunk", [])))

        # a MENTION is not plain text: the body renders the pill as its label, and which user was
        # addressed survives only in the html anchor and m.mentions. a bridge reads those to strip
        # the mention without mangling the words after it, so the fake has to be able to send them.
        html, mentions = fakematrix.mention(fakematrix.BOT_USER, fakematrix.BOT_DISPLAY_NAME)
        fake.inject(fakematrix.GROUP_ROOM, fakematrix.HUMAN,
                    f"{fakematrix.BOT_DISPLAY_NAME} how are you?", html, mentions)
        incr = http(base + "/_matrix/client/v3/sync?since=" + incr["next_batch"])
        ev = ((incr.get("rooms", {}).get("join", {}).get(fakematrix.GROUP_ROOM, {})
               .get("timeline", {}).get("events", []) or [{}])[0].get("content", {}))
        check("a mention injection carries formatted_body",
              fakematrix.BOT_USER in (ev.get("formatted_body") or ""))
        check("a mention injection carries m.mentions",
              fakematrix.BOT_USER in (ev.get("m.mentions", {}).get("user_ids") or []))

        profile = http(base + f"/_matrix/client/v3/profile/"
                       f"{urllib.parse.quote(fakematrix.BOT_USER, safe='')}/displayname")
        check("the bot has a profile display name",
              profile.get("displayname") == fakematrix.BOT_DISPLAY_NAME)

        # a receipt answers an empty 200, so the capture is the only way to see one.
        http(base + f"/_matrix/client/v3/rooms/{room_path}/receipt/m.read/%24evt1", "POST", {})
        got = fake.receipts()
        check("a read receipt is captured", any(
            r.get("room_id") == fakematrix.DM_ROOM and r.get("type") == "m.read" for r in got))
        check("the receipt names the event it acknowledges",
              any(r.get("event_id") == "$evt1" for r in got))

        fake.reset()
        check("reset clears captured sends", fake.sent() == [])
        check("reset clears captured receipts", fake.receipts() == [])

    print(f"\n{PASS} passed, {FAIL} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
