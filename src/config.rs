// command-line configuration. flags are hand-parsed to avoid a parser
// dependency for this small surface (mirrors fake-openai).

pub const DEFAULT_HOST: &str = "127.0.0.1";
// the fake seeds two rooms so bots can be driven immediately: one DM (bot + human,
// flagged m.direct) and one group room (bot + human + a third member).
pub const DEFAULT_BOT_USER: &str = "@per:fake.local";
// the bot's PROFILE display name. a real client expands a `@per` mention into a pill whose text
// is this, so a bridge that strips mentions has to fetch it -- serving one is what lets a test
// tell "the bot read its own name" apart from "it fell back to the localpart".
pub const DEFAULT_BOT_DISPLAY_NAME: &str = "Persona";
pub const DEFAULT_HUMAN: &str = "@tester:fake.local";
pub const DEFAULT_OTHER: &str = "@other:fake.local";
pub const DEFAULT_DM_ROOM: &str = "!dm:fake.local";
pub const DEFAULT_GROUP_ROOM: &str = "!room:fake.local";
// a third seeded room flagged m.room.encryption, for exercising the e2e relay path.
pub const DEFAULT_ENC_ROOM: &str = "!enc:fake.local";
// a DM with `other` that NOTHING is ever injected into: the room a bot writes to first. it is
// flagged m.direct, so a client's dm lookup finds it without a create, which is what lets a test
// drive an agent-initiated message the way a proactive dm really arrives.
pub const DEFAULT_QUIET_ROOM: &str = "!quiet:fake.local";

const FLAG_HOST: &str = "--host";
const FLAG_PORT: &str = "--port";
const FLAG_BOT_USER: &str = "--bot-user";
const FLAG_BOT_DISPLAY_NAME: &str = "--bot-display-name";
const FLAG_HUMAN: &str = "--human";
const FLAG_OTHER: &str = "--other";
const FLAG_DM_ROOM: &str = "--dm-room";
const FLAG_GROUP_ROOM: &str = "--group-room";
const FLAG_ENC_ROOM: &str = "--enc-room";
const FLAG_QUIET_ROOM: &str = "--quiet-room";

pub struct Config {
    pub host: String,
    pub port: u16,
    pub bot_user: String,
    pub bot_display_name: String,
    pub human: String,
    pub other: String,
    pub dm_room: String,
    pub group_room: String,
    pub enc_room: String,
    pub quiet_room: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            host: DEFAULT_HOST.to_string(),
            port: 0,
            bot_user: DEFAULT_BOT_USER.to_string(),
            bot_display_name: DEFAULT_BOT_DISPLAY_NAME.to_string(),
            human: DEFAULT_HUMAN.to_string(),
            other: DEFAULT_OTHER.to_string(),
            dm_room: DEFAULT_DM_ROOM.to_string(),
            group_room: DEFAULT_GROUP_ROOM.to_string(),
            enc_room: DEFAULT_ENC_ROOM.to_string(),
            quiet_room: DEFAULT_QUIET_ROOM.to_string(),
        }
    }
}

pub fn parse_args(args: Vec<String>) -> Result<Config, String> {
    let mut c = Config::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        match key.as_str() {
            FLAG_HOST => c.host = take(&mut it, inline, FLAG_HOST)?,
            FLAG_PORT => c.port = parse_num(&take(&mut it, inline, FLAG_PORT)?, FLAG_PORT)?,
            FLAG_BOT_USER => c.bot_user = take(&mut it, inline, FLAG_BOT_USER)?,
            FLAG_BOT_DISPLAY_NAME => {
                c.bot_display_name = take(&mut it, inline, FLAG_BOT_DISPLAY_NAME)?
            }
            FLAG_HUMAN => c.human = take(&mut it, inline, FLAG_HUMAN)?,
            FLAG_OTHER => c.other = take(&mut it, inline, FLAG_OTHER)?,
            FLAG_DM_ROOM => c.dm_room = take(&mut it, inline, FLAG_DM_ROOM)?,
            FLAG_GROUP_ROOM => c.group_room = take(&mut it, inline, FLAG_GROUP_ROOM)?,
            FLAG_ENC_ROOM => c.enc_room = take(&mut it, inline, FLAG_ENC_ROOM)?,
            FLAG_QUIET_ROOM => c.quiet_room = take(&mut it, inline, FLAG_QUIET_ROOM)?,
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(c)
}

fn take(
    it: &mut std::vec::IntoIter<String>,
    inline: Option<String>,
    flag: &str,
) -> Result<String, String> {
    inline
        .or_else(|| it.next())
        .ok_or_else(|| format!("{flag}: requires a value"))
}

fn parse_num<T: std::str::FromStr>(s: &str, flag: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("{flag}: invalid number '{s}'"))
}

pub fn usage() -> String {
    let mut u = String::new();
    u.push_str("fake-matrix - standalone fake matrix homeserver for testing bots/bridges\n\n");
    u.push_str("usage: fake-matrix [flags]\n\n");
    u.push_str("flags:\n");
    u.push_str(
        "  --host <addr>        bind host (default 127.0.0.1; 0.0.0.0 to reach from a container)\n",
    );
    u.push_str("  --port <n>           bind port, 0 = free port (default 0)\n");
    u.push_str("  --bot-user <id>      the bot's matrix user id (default @per:fake.local)\n");
    u.push_str(
        "  --bot-display-name <name>  the bot's profile display name (default Persona)\n",
    );
    u.push_str(
        "  --human <id>         the human in the seeded rooms (default @tester:fake.local)\n",
    );
    u.push_str(
        "  --other <id>         a third member of the group room (default @other:fake.local)\n",
    );
    u.push_str("  --dm-room <id>       the seeded direct room id (default !dm:fake.local)\n");
    u.push_str("  --group-room <id>    the seeded group room id (default !room:fake.local)\n");
    u.push_str("  --enc-room <id>      the seeded encrypted room id (default !enc:fake.local)\n");
    u.push_str("  --quiet-room <id>    a dm with `other` nothing is injected into (default !quiet:fake.local)\n");
    u.push_str("  --help               show this help\n\n");
    u.push_str("on startup prints: base_url=http://<host>:<port>\n");
    u
}
