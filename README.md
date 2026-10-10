# fake-matrix

a standalone fake matrix homeserver for testing matrix bots and bridges without a
real homeserver. a harness launches it as a subprocess, points a bot's
`homeserver` at it, injects an inbound message, and inspects what the bot sent back.

it is the matrix analogue of [fake-openai](../fake-openai): a language-agnostic
process that is programmable (a test injects the traffic) and observable (every
outbound send is captured over an http admin plane).

## why it works without e2e crypto

matrix-sdk clients usually compile end-to-end encryption in with no off switch,
but message encryption only engages on a room carrying an `m.room.encryption`
state event. this fake presents ONLY unencrypted rooms and never emits that
event, so every send/receive stays plaintext. the one crypto concession is the
first-sync device-key upload, answered with a stub.

## build

```
make            # cargo build
make precommit  # lint + tests + build + python self-test
```

deterministic builds use `mise` (rust pinned in [mise.toml](mise.toml)). the only
runtime dependencies are `tiny_http` and `serde_json`.

## run

```
fake-matrix --port 0
```

on startup it prints a single line a parent process reads to learn the url (no
`/v1` suffix -- a homeserver base url is the bare root):

```
base_url=http://127.0.0.1:54321
```

`--port 0` picks a free port; `--host 0.0.0.0` binds all interfaces so a container
can reach it. point a bot at the printed url, for example:

```
export BRIDGE_HOMESERVER=http://127.0.0.1:54321
export BRIDGE_USER_ID=@per:fake.local
export BRIDGE_ACCESS_TOKEN=fake_token
```

## seeded rooms

two unencrypted rooms exist at startup so a bot can be driven immediately:

- a DM (`--dm-room`, default `!dm:fake.local`): the bot + the human (`--human`),
  flagged direct via `m.direct` account data.
- a group room (`--group-room`, default `!room:fake.local`): the bot + the human +
  a third member (`--other`).

## matrix client-server endpoints

the minimum a bot/bridge client exercises, all plaintext:

```
GET  /_matrix/client/versions
GET  /_matrix/client/v3/account/whoami        -> {user_id, device_id}
POST /_matrix/client/v3/keys/upload           -> stub key counts
POST /_matrix/client/v3/keys/query            -> empty
GET  /_matrix/client/v3/sync                   -> initial state + long-poll for injected events
PUT  /_matrix/client/v3/rooms/{room}/send/{type}/{txn}   -> captures the send
PUT  /_matrix/client/v3/rooms/{room}/typing/{user}
GET  /_matrix/client/v3/rooms/{room}/members
GET  /_matrix/client/v1/media/download/{server}/{id}     -> the bytes of injected media
GET  /_matrix/media/v3/download/{server}/{id}            -> the same, at the older route
```

any other matrix path returns an empty `200` so incidental sdk probes do not error.

## admin plane (`/_fake`)

how a test drives it (shares the server port):

```
POST /_fake/inject   {room_id, sender, body}  -> queue an inbound m.text into a room
                     + media: {msgtype, filename, mimetype, hex}
                                              -> the message is that file, and body its caption
GET  /_fake/sent                              -> [{room_id, type, msgtype, body, content}, ...]
GET  /_fake/rooms                             -> the seeded rooms
POST /_fake/reset                             -> clear the timeline + captures
GET  /_fake/health
```

## python client

`clients/python/fakematrix.py` (stdlib only) spawns the binary and wraps the admin
plane, mirroring `fakeopenai.py`:

```python
import fakematrix
with fakematrix.FakeMatrix() as fake:
    # point a bridge at fake.base_url (BRIDGE_HOMESERVER), drive it, then:
    fake.inject(fake.DM_ROOM, "@tester:fake.local", "hey per")
    replies = fake.wait_sent(1)
    assert "per" in replies[0]["body"].lower()
```
