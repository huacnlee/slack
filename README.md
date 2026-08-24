# Slack for the desktop, in Rust

A native Slack client built with [GPUI](https://github.com/zed-industries/zed)
and [gpui-component](https://github.com/longbridge/gpui-component). It talks to
the Slack Web API directly with a user token, following the same approach as
[Bottelet/omarchy-slack](https://github.com/Bottelet/omarchy-slack): no
gateway, no proxy, no shared client secret.

```
┌──────────────┬───────────────────────────────┬──────────────┐
│  #general  3 │  #general                     │  Thread      │
│  #design     │  ─────────────────────────────│  ────────────│
│  #eng      1 │  ▣ Ada   14:32                │  ▣ Ada 14:32 │
│              │    Deploy is out :tada:       │    Deploy is │
│  Ada Lovelace│    👍 3  🎉 1     ↳ 4 replies │    out       │
│  Alan Turing │                               │              │
│              │  ┌──────────────────────────┐ │  ┌─────────┐ │
│  ● Active    │  │ Message #general    Send │ │  │ Reply…  │ │
└──────────────┴───────────────────────────────┴──────────────┘
```

## What it does

- **Conversations** — channels, private channels, group DMs, and direct
  messages in one collapsible tree, with starred conversations pinned above
  the rest, a filter field, and unread markers.
- **Transcript** — avatars, grouped consecutive messages, day separators, an
  unread divider, edited markers, quiet system notices, and inline image
  attachments.
- **Slack `mrkdwn`** — bold, italic, strikethrough, inline and fenced code,
  quotes, lists, links, `<@user>` and `<#channel>` mentions, `<!here>`
  broadcasts, and escaped entities, all parsed and rendered rather than shown
  raw.
- **Emoji** — Unicode short names plus the workspace's custom emoji, including
  `alias:` chains, in message bodies and reactions.
- **Writing** — Enter sends, Shift+Enter breaks the line, per-conversation
  drafts survive switching, and files can be attached.
- **Threads** — a resizable reply pane beside the transcript.
- **Reactions** — a picker of frequent emoji; click an existing reaction to
  toggle your own.
- **Message commands** — edit and delete your own messages, copy a permalink.
- **Presence and notifications** — set yourself active or away, pause
  notifications for a while, resume them.
- **Search** — full-text search across the workspace; a result opens its
  conversation.
- **Quick switcher** — `⌘K` to jump to any conversation by name.
- **People** — hovering an avatar, a name, or a mention shows who they are;
  clicking opens their profile in a sheet.
- **Offline** — the workspace, the directory, the emoji, recent messages, and
  image thumbnails are all cached, so the client opens and stays readable with
  no network.
- **Light and dark themes**, following the system appearance at launch.

## Getting a token

The client signs in with a Slack **user token** (`xoxp-…`) from an app you
control. There is deliberately no "Sign in with Slack" button: the OAuth
code-for-token exchange needs a client secret, which means it has to run on a
server that then sees your token. A desktop client should not require that.

Slack can build an app from a pasted manifest, so this is one paste rather than
a scope at a time in a web form:

1. Go to [api.slack.com/apps](https://api.slack.com/apps) → **Create New App**
   → **From an app manifest**, and pick your workspace.
2. Paste [`manifest.yml`](manifest.yml) — or press the copy button beside
   step 1 on the client's sign-in screen, which holds the same text.
3. **Create**, then **Install to Workspace** and approve.
4. Under **OAuth & Permissions**, copy the **User OAuth Token** (`xoxp-…`).
5. Paste it into the client.

To add these to an app you already have, open **Scopes it requests** on the
sign-in screen and copy the list as one line, or read them from `manifest.yml`.

The token is stored in your operating system's keychain, and **Sign out**
removes it.

### Not being asked for a password every launch

macOS ties keychain access to a binary's code signature, so a freshly compiled
binary is a new application to the keychain and prompts on every launch. During
development, put the token in a `.env` file instead — see `.env.example`:

```sh
cp .env.example .env      # then paste your token into it
```

`SLACK_TOKEN` is used as-is and nothing is written to disk or the keychain.
`.env` is git-ignored, but it is a plaintext credential on disk: any process
running as you can read it, which the keychain would not have allowed.

### What each scope buys

| Scopes | Without them |
| --- | --- |
| `channels:read` `groups:read` `im:read` `mpim:read` | no sidebar |
| `channels:history` `groups:history` `im:history` `mpim:history` | no transcript |
| `chat:write` | read-only |
| `channels:write` `groups:write` `im:write` `mpim:write` | other Slack clients keep showing what you read here as unread |
| `users:read` | names and avatars stay as raw IDs |
| `reactions:read` `reactions:write` | no reactions |
| `emoji:read` | custom emoji render as `:shortcodes:` |
| `users:write` `dnd:read` `dnd:write` | no presence or notification pausing |
| `files:write` | attaching fails |
| `search:read` | search reports that it cannot run |

`manifest.yml` is generated from the scope list in
`crates/slack-ui/src/manifest.rs`; a test fails if the two drift apart.

## Running it

```sh
cargo run --release
```

Debug builds work too, and are what `cargo run` gives you by default.

## Keyboard

| Keys | Command |
| --- | --- |
| `⌘K` | Jump to a conversation |
| `⌘F` | Search messages |
| `⌘R` | Reload conversations and directory |
| `⌘⇧T` | Switch between light and dark |
| `Enter` | Send |
| `⇧Enter` | Line break |
| `Esc` | Close the thread pane |

## How it is put together

```
crates/
├── slack-api/     The Slack Web API: transport, wire shapes, mrkdwn,
│                  emoji resolution, the on-disk cache, and where the token
│                  is stored. Knows nothing about windows.
├── slack-ui/      Views and application state, grouped by capability:
│   ├── auth/        sign-in
│   ├── workspace/   the shared store, sidebar, shell, quick switcher
│   ├── channel/     transcript, message rows, composer, threads, attachments
│   ├── people/      person cards and the profile sheet
│   └── search/      message search
└── slack-app/      The binary: window, assets, menu bar, `.env`.
```

Four decisions are worth knowing before reading the code.

**Transport is executor-agnostic.** `slack-api` runs its HTTP on a private
Tokio runtime and returns results over a channel, so every method can be
awaited from GPUI's executor without a reactor of its own.

**One store, many views.** `WorkspaceStore` owns the client, the conversation
list, the directory, and the emoji index. Views observe it; none of them keeps
a second copy.

**Cache first, everywhere.** Enumerating this workspace over the network takes
seconds — a thousand conversations and a thousand members is a dozen paged
requests. So the store opens from disk and the first frame already has a
complete, sorted sidebar; the network refresh runs behind it in stages and
writes through. On a real workspace of 1013 conversations and 1077 members
that is **6ms from cache** against about three seconds from the network. The
same mechanism is what makes the client work with no network at all.

**Unread is derived, not fetched.** Slack returns no unread count and no
latest-message timestamp to an OAuth user token: `conversations.info` gives
only `last_read`, and the bulk `users.counts` endpoint refuses this token type
outright. So a budgeted background sweep learns the newest timestamp per
conversation, compares it against the read marker, and persists both. That is
why the sweep exists, why its results are cached, and why an unread
conversation is marked with a dot rather than a number the client would have
had to invent.

## What it does not do

- **No real-time socket.** Slack's RTM API is closed to new apps, and Socket
  Mode needs an app-level token and an app manifest that a user token cannot
  provide. New messages arrive by polling the open conversation, starting at
  six seconds and backing off to a minute the first time Slack says that is too
  often.
- **No unread counts**, for the reason above — only whether a conversation has
  anything new.
- **Starring is local** unless the token carries `stars:read`. Slack's stars
  are read-only to an OAuth app, so a star set here is remembered by this
  client and merged with whatever Slack reports.
- **Attachments open in the browser**, where your Slack session already exists.
  Image thumbnails are fetched and cached so they render inline.
- **No huddles, calls, canvases, or workflows.**
- **One workspace at a time.**

## Tests

```sh
cargo test --workspace
```

The parser, the emoji index, the transcript window and its notice grouping,
the cache, the workspace snapshot schema, the unread derivation, the sweep's
priority order, the `.env` reader, the token validator, and timestamp
formatting are covered by unit tests; those are the parts where a silent
regression would be hard to see on screen.

`cargo run -p slack-api --example probe` runs the same API calls the client
depends on against your live workspace and reports the shape of what came
back — the fastest way to tell a broken client from a missing scope. Add
`--write` to exercise posting, editing, reacting, and uploading; it writes to
`#slack-gpui-test` if that channel exists and to your own DM otherwise.

## Licence

MIT.
