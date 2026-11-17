# pitwall

**The agentic IDE.** Write what you want, run agents on it in parallel, review what they did and merge it, all in one window.

![pitwall: the project tree and agents on the left, an agent and a file open in two cards](docs/screenshot.png)

Running more than one coding agent usually means a terminal tab per agent, a chat window for planning, lazygit for the diffs and an editor for the fixes, and keeping in your head which agent is waiting on you. Agents edit the same checkout and trip over each other. Reviewing means reading a diff and typing your notes back into a prompt.

pitwall puts that whole loop in one place:

1. **Write the brief.** Say what you want in a Markdown brief made from a template (feature, bug, refactor, UI, spike). Let Claude fill in the rest from your code while you watch it write: the problem, the files involved, the steps, the open questions.
2. **Hand it to an agent.** Claude Code, Codex or another agent CLI starts on its own git worktree and branch, so agents in parallel never touch your checkout or each other's.
3. **Watch them.** Every agent in every project is listed by what needs you: waiting on an approval, asking a question, done, working. Each row shows how far through its brief it is and how much it has changed. Commands that change things wait for your approval; destructive ones are refused.
4. **Review.** Read the agent's diff against where it started, comment on lines and send the comments back to it as one message, or throw away a hunk. Every step it took is a snapshot you can revert to.
5. **Accept.** One step commits what it left, merges its branch into the one it came from and offers to clean up its worktree.

It's also a real terminal, drawn on the GPU, so the agents run in their own TUIs and your shells, vim and lazygit sit beside them in tiled cards. A background daemon owns every terminal: close the window and everything keeps running; restart the machine and your agents come back in their folders and pick up their sessions.

### What's in it

- **Agents in their own worktrees**, with an undo timeline of snapshots and an approval bar for the commands they want to run.
- **A command policy that only ever adds restrictions.** pitwall denies what your rules deny and queues what needs a look; it never quietly allows something on its own reading of a shell command.
- **Briefs** linked to the agents building them: the agent ticks off each step as it finishes it, and **Review →** takes you to its diff.
- **Review built for agents:** diff against the agent's base, line comments sent back to it, discard a hunk, Accept to commit and merge.
- **Agents that talk to each other** through the `pitwall` command: list, message, wait on and read one another.
- **Projects first.** The sidebar is a tree of your projects, each with its Changes, Files, briefs and terminals.
- **Remotes.** SSH sessions to your servers as terminal cards: groups, Connect all side by side, typing broadcast to every box, a command run across many, and agents running on a server listed with your own.
- **API keys, locked away.** A Keys tab keeps tokens encrypted (AES-256-GCM) behind their own password, which revealing or copying one always asks for.
- **Git and an editor built in**: lazygit-style staging and commits, a file tree with an icon for every language, a built-in editor with Markdown preview, or nvim, helix, VS Code and friends.
- **A terminal that holds up**: tabs and tiles, scrollback that survives restarts, text that rewraps when a card narrows, mouse reporting, IME, OSC 52, synchronized output, the kitty keyboard protocol for Shift+Return in Claude Code and Codex.

## Install

On macOS, with Homebrew:

```sh
brew install --cask jason-shen/tap/pitwall   # Pitwall.app, and `pitwall` on your PATH
```

From source you need Rust 1.80+ and `git`. macOS and Linux are supported. Windows isn't yet, since the daemon uses Unix sockets.

```sh
make install                             # macOS: Pitwall.app in /Applications, `pitwall` in ~/.local/bin
cargo install --path crates/pitwall       # elsewhere: the daemon, CLI and terminal UI
cargo install --path crates/pitwall-gui   # and the app
pitwall-gui ~/code/my-app                 # or run `pitwall-gui` inside a repo
```

The daemon starts on its own the first time anything needs it. Opened from Spotlight or the Dock, the app loads your login shell's environment (`PATH`, API keys), so `claude` and `codex` are found.

**Renderer:** pitwall draws with wgpu (Metal on macOS) by default. `cargo install --path crates/pitwall-gui --no-default-features --features glow` builds an OpenGL version instead: 10.5 MB against 14.5 MB, with the same memory use and no difference at idle.

## A quick tour

1. **Open a project.** Run `pitwall-gui` in a repo, or press **+** on the sidebar's Projects tab and pick a folder. A new project opens with your default agent and a terminal. With nothing open, the app starts on an empty stage rather than a shell in your home folder.
2. **Write a brief.** **+ Brief** under the project (⌘⌥B), pick a template and a title, and write what you want on the **Ask:** line. **✦ Finish with AI** (⌘⌥F) has Claude read the code and fill in the rest.
3. **Hand it off.** **Hand to agent →** (⌘⌥↵) opens the New agent form with the brief as the task. Or skip the brief: **+ Agent** (⌘⇧N), or type a task into the command bar (⌘⇧P) and press Enter.
4. **Watch it.** It appears under AGENTS with its status, what it's doing and how far through the brief it is. When it needs you (an approval, a question, a finished turn), it moves to the top, the project shows "1 waiting", and the dock bounces if pitwall is in the background. ⌘⇧A jumps to the next one that needs you.
5. **Review it.** **Review →** on the brief, or **± Changes** on the agent: its diff against where it started. `r` comments on a line, **send to agent** hands your comments back, `d` throws away a hunk, and ⌘⇧U shows every step it took, to revert to any of them.
6. **Accept it.** `m` commits what it left, merges its branch into yours and offers to remove the worktree.

## The window

```
┌ ⌕ Search or start a task  ⌘⇧P ┐┌ ● server   ● reviewer  ● tests   +     ± Changes 6  ▤ Files ┐
│ Projects │ Remotes │ Keys   + ││┌ pitwall › reviewer › Agent ──┐┌ pitwall › README.md ─────────┐│
│ ▾ ● pitwall                    │││                             ││                             ││
│     ± Changes        6 files  │││   the agent's terminal      ││   the file, in your editor  ││
│     ▤ Files                   │││                             ││                             ││
│     · README.md               │││                             ││                             ││
│     ❯ server                  │││                             ││                             ││
│     ○ reviewer       working  │││                             ││                             ││
│     ❯ tests                   ││└─────────────────────────────┘└─────────────────────────────┘│
│     + Agent  + Terminal       ││                                                             │
│ ▸ ● augur          1 waiting  ││                                                             │
│ AGENTS             priority + ││                                                             │
│ ● pitwall · reviewer           ││                                                             │
│   Reviewing the login fix     ││                                                             │
│ ⚙ Settings  ? Shortcuts  ● daemon                                                            │
└───────────────────────────────┘└─────────────────────────────────────────────────────────────┘
```

The sidebar has three tabs: **Projects**, **Remotes** and **Keys**. The **+** at the end of the row adds to whichever is showing. A dot on a tab you're not on says something there wants you (amber) or a remote session is open (green).

### Projects

Every project (a git repo, or a folder that isn't one) is a branch of the tree: its **Changes**, its **Files**, files you've opened, then its terminals in the order you opened them. A terminal running an agent stays in its place and shows the agent's status. **+ Agent** and **+ Terminal** sit at the end of each project. Click a project to switch to it (⌘1–9, or ⌘⇧J / ⌘⇧K to step), and use ▸ to fold one away; the tree doesn't fold or reorder itself. Right-click a project and **Remove project…** to take it off the sidebar: its terminals and agents close (agents' worktrees and branches stay), and the folder itself isn't touched. Adding the folder again with **+** brings it back.

Terminals follow `cd`. A terminal that moves into another repo moves to that project.

### Agents

AGENTS lists the agents in every project, one row each, as herdr does: a status dot, `project · name`, and the title the agent gives its terminal (Claude Code's summary of the session), or what it's waiting on. Sort by **priority** (approve, blocked, done, working, idle) or **recent**. Clicking an agent switches to its project and shows it. An agent with its own worktree shows how much it has changed so far (files, lines added and removed), so you can tell finished agents apart without opening each. An agent handed a brief shows the brief's title and how many of its steps are ticked. Right-click for its Changes, its brief, Accept, kill or remove.

Any terminal running claude, codex, opencode, gemini, aider, goose, amp and other agent CLIs counts as an agent, whether pitwall started it or you typed it. When one is blocked on a numbered prompt, its choices show as buttons under it in the tree, so you can answer without opening it.

An agent running on a server, in a remote session, is listed too, as `machine · claude` with a **remote** badge. Locally all pitwall can see is `ssh`, so it reads the agent off the session's screen and title the way keel does: the agent's name leading the title (Claude Code's `✳ <topic>` counts), or its name on screen together with its own UI.

### Tabs and cards

The open project's terminals and agents are tabs along the top of the stage (or the bottom: Settings → Tabs and cards). Click a tab to show it in the focused card; double-click to rename; middle-click or × to close a terminal; ⌘⇧[ / ⌘⇧] step through them.

Whatever is on screen sits in a **card**. ⌘⇧D splits, and you can tile as many cards as you like:

- drag a tab onto another tab, or onto a card, to tile it beside that one;
- drag a card's title bar onto another card to swap them;
- drag the gap between cards to resize them, double-click it to even them out;
- ⌃⌥H / J / K / L move between cards, ⌃⌥⇧H / J / K / L swap, ⌃⌥Z maximizes, ⌃⌥W closes (rebind them in Settings → Shortcuts). They're on Ctrl+Option so plain Option keys reach the programs in the terminal;
- right-click a title bar to maximize, swap, broadcast or close.

**Broadcast** (⌘⇧B) sends your typing to every terminal card at once. While it's on, or once two remote sessions share the screen, a strip above the cards says where typing goes and turns it on or off, and broadcasting cards get an orange border.

### Remotes

The Remotes tab lists your SSH servers: the ones under `hosts` in config.toml and every `Host` entry in `~/.ssh/config`. Each shows a name you can change (double-click) and its IP. Click one to open a session to it as a terminal card. Sessions use your own `ssh`, so your ssh config, ProxyJump and SSH agent all apply, and they get a tab strip of their own.

- **+ → Add a remote…** takes an address, and optionally a name, user, port, key file, tags and a group, and writes it into config.toml. Right-click a remote to edit it, open a new session, run a command on it, copy its address or remove it. Removing one from `~/.ssh/config` only stops pitwall listing it; the file isn't touched.
- **Groups.** **+ → New group** makes a folder; drag a remote onto one, or right-click → **Move to**. Anything in no group sits under **Ungrouped**. Right-click a group to connect to or run a command on all of it, rename it or delete it (its remotes go back to Ungrouped). Groups and names are kept in `state.json`, so `~/.ssh/config` hosts can be grouped too.
- **Connect all** opens every remote, or a group, tiled side by side; turn on broadcast and type into all of them at once.
- **Run…** sends one command to many remotes and shows each one's exit code, time and output. It goes through the same policy as agents' commands, so anything that changes things asks first.
- A session that ends keeps its screen, so you can read why, with **Reconnect** and **Close** under it.

### API keys

The Keys tab keeps tokens for other services (OpenAI, Stripe, GitHub). The first time, you set a password for them. Each key is sealed with AES-256-GCM under a random key, and that key is wrapped with one derived from your password (PBKDF2-SHA256, 600,000 rounds), the same scheme keel uses. `~/.pitwall/apikeys.json` is readable only by you and never holds a key in the clear.

Each row shows the name, the service and the last four characters. **Reveal** and **Copy** ask for the password every time: a revealed key hides again after 30 seconds, and a copied one is cleared from the clipboard after 30 seconds if it's still there. For five minutes after you type the password, new keys can be saved without asking again. **+** also changes the password and locks the keys at once. A forgotten password can't be recovered: **Forgot the password…** deletes every key so you can start again.

### Changes

lazygit's layout and keys for the project's repo: Status, Changed files, Branches, Commits and (for agents) the undo history, with the diff beside them. Stage and unstage whole files or single hunks, discard, commit, check out a branch, and double-click a diff line to open the file there. An agent with its own worktree has its own Changes, with **vs base** (everything the agent did on its branch).

Reviewing an agent's work: on any line of its diff, **💬 comment** (`r`) writes a review comment. Comments collect per agent, marked in the diff, and **send N to agent** types them all into its prompt as one message, so it can address them in its next turn. When you're happy, **✓ accept** (`m`) commits whatever it left uncommitted in its worktree, with a message you can edit (the brief's title by default), and merges its branch into the branch it forked from. Your checkout has to be on that branch, clean, and without untracked files where the agent's work would land; Accept checks all of that before it commits anything. Afterwards pitwall offers to remove the agent, its worktree, its snapshots and (now that it's merged) its branch. To throw away part of what it did first, **discard hunk** (`d`) undoes one hunk in its worktree, in vs base as well as in Unstaged. Git work runs in the background, so a slow pre-commit hook doesn't freeze the window.

### Files and editors

The project's file tree, with a Nerd Font icon for each language and file type (Rust, TypeScript, Python, Go, Markdown, Docker, git files and around 70 more), coloured as editors colour them. The symbols font is bundled, so the icons show whatever terminal font you pick. Clicking a file opens it in the editor you choose in Settings:

- the **built-in editor** (syntax highlighting, selection, undo, find, go to line), which reloads live when an agent changes the file;
- a **terminal editor** (nvim, vim, helix, micro, nano) running inside the card;
- a **desktop editor** (VS Code, Cursor, Zed, Sublime, JetBrains IDEs) opened at the line, while the built-in editor follows along and reloads as you save;
- or `$VISUAL` / `$EDITOR`.

Markdown files always open in the built-in editor, whatever you pick here (see below).

⌘-click a path in a terminal (`src/app.rs:42` too) to open it the same way. Right-click a file, in the tree or where it's open, and **Move to Trash…** to delete it; it goes to the Trash, so the Finder's Put Back undoes it.

### Briefs

A brief is a short Markdown doc you write before handing work to an agent. **+ Brief** under a project opens a popup with a title and six templates made for this kind of work:

| Template | For |
|---|---|
| ✦ Feature | Something new the app should do: problem, scope, approach, choices, steps |
| ✕ Bug fix | Something broken: what happens, how to see it, cause, fix |
| ↻ Refactor | Change the shape, keep the behaviour, and prove nothing broke |
| ◐ UI polish | How a screen should look and feel, with its states |
| ◇ Spike | Find out first: options and a recommendation |
| □ Blank | A title and an Ask line |

The cursor lands on the **Ask:** line, where you say what you want in your own words. Fill in as much as you know, then:

- **✦ Finish with AI** runs `claude -p` in the background with read-only tools (Read, Grep, Glob). It reads the code and fills in the rest, following the guide comment at the top of the template. Its reply streams straight into the brief, so you watch it write in the preview; the toolbar shows what it's reading meanwhile, with **Stop** to cancel. The brief is locked while it writes; if you stop it or it fails, the brief goes back to exactly what you had. It can't ask you anything mid-run, so choices that change what gets built come back as options and a recommendation under Choices (or Open questions).
- **Hand to agent →** opens the New agent form with the brief as its task and its title as the name, ready to start on its own branch. The agent stays linked to the brief: it ticks each step as it finishes it with `pitwall brief tick <n>` (the daemon writes the brief, so this works for sandboxed agents too, and it's allowed without asking), the brief's row shows who's building it and how far along it is, and the toolbar's **Review →** opens the agent's Changes.

Briefs are listed under their project and saved in `.pitwall/briefs/` in the project, which is git-ignored by default (delete `.pitwall/.gitignore` to commit them). Right-click one and **Move to Trash…** to delete it.

### Markdown

Any Markdown file, briefs included, opens with **Edit** and **Preview**. Preview puts the rendered page next to the editor, and the two scroll together: scroll either one and the other follows. It renders headings, lists, task boxes, code blocks, quotes, tables and links; briefs open in Preview.

Markdown is always edited in the built-in editor, the way inkterm does it, with quiet colouring: headings, list markers and task boxes, bold, code and links stand out and the prose stays plain text; guide comments are dimmed. Lists, tasks and quotes carry on when you press Enter, and Enter on an empty item ends the list. **⌘T** turns the line into a task or ticks it, **⌘B** makes the word bold and **⌘E** makes it code (press again to undo either). A colon at the end of a line doesn't indent the next one.

### Approvals and undo

pitwall hooks into Claude Code for that session only, through `--settings`. Codex is hooked the same way (`hooks = "codex"`, the default); Codex asks you once to trust pitwall's hook. Before a Codex agent starts, pitwall checks that this Codex reads the hook config, and starts it without the hook if not. Commands that change things wait in an approval bar (**Approve** ⌘⇧Y, **Reject** ⌘⇧R), and destructive ones are refused. pitwall only adds restrictions: it says "allow" only for your own allow rules, and leaves everything else to the agent's own permission prompt, so a command it misreads can't run silently. (`auto_allow_read_only = true` under `[hooks]` lets commands it judges read-only through without the agent asking.) ⌘⇧Y and ⌘⇧R only answer the agent you're looking at. The rules are yours to change in the config. An allow rule covers one command: in `cargo test && rm -rf ~`, each part is judged on its own and the strictest answer wins. It doesn't cover output sent into a file (`cargo test > ~/.zshrc`) or flags that make the tool run another program (`cargo --config`, `go test -exec`, npm's `--script-shell`). Deny rules hold anywhere in the line, including behind `FOO=1`, `sudo` or `git -C dir`, and inside `bash -c '…'`, which is judged by the script it runs. Agents' own `pitwall agents`, `read`, `wait` and `tell` are allowed.

Finish with AI and agents rewriting an open file don't cost you your text: the version before is one ⌘Z away in the editor.

Every agent also has a shadow-git timeline: a snapshot before each tool call that can change something (read-only commands like `ls` or `git log` are skipped), after file changes and at the end of each turn. Diff any step, revert to it, and undo a revert.

### Notifications

Agents' own notifications (OSC 9 / 777 / 99; Claude Code sends them with `preferredNotifChannel` set to `iterm2` or `ghostty`) show as toasts. While pitwall is in the background, an agent that needs you or finishes also posts a system notification (Settings → New agents turns that off).

## Agents working together

Agents coordinate through the `pitwall` command, which every pitwall terminal has on its `PATH`. Each terminal also knows its own id as `$PITWALL_TASK_ID`.

```sh
pitwall agents                          # every agent and terminal: status, folder, what it's doing
pitwall tell reviewer "check the login fix, then tell me"
pitwall wait reviewer                   # block until its turn ends
pitwall read reviewer -n 60             # the end of its screen
pitwall brief                           # the steps of the brief you were handed
pitwall brief tick 2                    # mark step 2 done (untick to undo)
```

`pitwall tell` types the message into the other agent's prompt and submits it, marked with who it's from and how to answer:

```
[message from agent builder; reply with: pitwall tell builder "..."] check the login fix, then tell me
```

Claude Code agents started by pitwall are told all of this in their system prompt. For an agent you started yourself in a terminal, ask it to run `pitwall guide`. Every message shows as a toast in the app and is kept in the audit log.

## Shortcuts

Every command has a shortcut, and every shortcut can be changed: in **Settings → Shortcuts** click one and press the new keys (Esc cancels, Backspace leaves the command without one), or edit `[keys]` in config.toml:

```toml
[keys]
command_bar = ["cmd+shift+p", "cmd+p"]   # a list gives a command more than one
new_agent = "cmd+shift+n"                # cmd is ⌘ on macOS and Ctrl elsewhere
card_left = "ctrl+alt+h"
clear_terminal = ""                      # no shortcut
```

Clashes are flagged in Settings, and **⌘⇧/** lists everything as it's set now. The defaults follow keel: app commands on **⌘⇧** (**Ctrl+Shift** on Linux), so plain ⌘ and every Ctrl key stay with the terminal (Ctrl+C still interrupts).

| | macOS | Linux |
|---|---|---|
| Command bar: open a file, jump to an agent, start a task | ⌘⇧P or ⌘P | Ctrl+Shift+P |
| New agent / terminal / project / brief | ⌘⇧N / ⌘⇧T / ⌘⌥N / ⌘⌥B | Ctrl+Shift+N / T, Ctrl+Alt+N / B |
| Next agent that needs you | ⌘⇧A | Ctrl+Shift+A |
| Approve / reject this agent's pending command | ⌘⇧Y / ⌘⇧R | Ctrl+Shift+Y / R |
| Brief: finish with AI / hand to agent | ⌘⌥F / ⌘⌥↵ | Ctrl+Alt+F / Enter |
| Changes / Files / undo history (again to go back) | ⌘⇧G / ⌘⇧O / ⌘⇧U | Ctrl+Shift+G / O / U |
| Markdown: edit or preview | ⌘⌥P | Ctrl+Alt+P |
| Split / close the focused card or open file | ⌘⇧D / ⌘⇧W | Ctrl+Shift+D / W |
| Next / previous tab | ⌘⇧] / ⌘⇧[, Ctrl+Tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Rename the tab / close the terminal | ⌘⌥R / ⌘⌥W | Ctrl+Alt+R / W |
| Switch project / previous, next | ⌘1–9 / ⌘⇧J, ⌘⇧K | Ctrl+1–9 / Ctrl+Shift+J, K |
| Broadcast typing to every terminal card | ⌘⇧B | Ctrl+Shift+B |
| Clear the terminal | ⌘K | none |
| Hide or show the sidebar | ⌘⇧E | Ctrl+Shift+E |
| Font bigger / smaller / reset | ⌘= / ⌘- / ⌘0 | Ctrl+= / - / 0 |
| Settings / all shortcuts | ⌘, / ⌘⇧/ | Ctrl+, / Ctrl+Shift+/ |

**Cards:** ⌃⌥H / J / K / L focus, ⌃⌥⇧H / J / K / L swap, ⌃⌥Return new terminal in its own card, ⌃⌥W close, ⌃⌥Z maximize.

The keys inside a card (the terminal's, the editor's and Changes', below) are fixed.

**Terminal:** ⇧Return is a newline in Claude Code and Codex, which ask for distinct keys (the kitty keyboard protocol), full screen or not; at a shell prompt it's a newline too, and in vim or less it's Return · Option is Meta, so Option shortcuts and ⌥B / ⌥F work, while the characters your layout types with Option (`@`, `{` …) still type (Settings → Terminal turns Meta off) · ⌥← / ⌥→ jump words · ⌘C / ⌘V copy and paste (Ctrl+Shift+C / V on Linux) · selections stay on their text as output scrolls, and dragging past the edge scrolls · ⌘F find in the scrollback · ⌘↑ / ⌘↓ previous / next command · ⌘K clear · ⌘-click a link or path · double-click a word, triple-click a line · Shift+PageUp / PageDown scroll back · mouse reporting for vim, htop and tmux (hold Shift to select instead) · right-click for copy, paste, copy the last command's output and snippets (Shift+right-click when the program takes the mouse). Programs can set the cursor shape, put text on the clipboard (OSC 52) and draw in italics; input methods for Chinese, Japanese and Korean work, and redraws a program marks as one update are drawn whole. Scrollback comes back when pitwall restarts. Making a card narrower or wider rewraps what's in it, scrollback included, so long lines aren't cut.

**Changes:** `1`–`5` or `h` / `l` pick a panel · `j` / `k` move, the diff follows · Enter into the diff, Esc back · space stage or unstage, `a` everything, `d` discard, `e` edit · `c` commit · in the diff: `n` / `p` next or previous hunk, `s` / `u` stage or unstage the hunk · `b` diff against the agent's base · `r` comment on the line · `d` discard the hunk · `m` accept (commit and merge) · undo history: `r` revert to here, `n` checkpoint.

**Built-in editor:** ⌘S save · ⌘Z / ⌘⇧Z undo, redo · ⇧ with the arrows, Home/End or a click selects, or drag; ⌘A all · ⌘C / ⌘X / ⌘V on the selection (the whole line when none) · Tab / ⇧Tab indent or dedent the selected lines · ⌘F find, ⌘G next · ⌘L go to line · ⌘K delete line · ⌘R reload from disk · in Markdown: ⌘T task, ⌘B bold, ⌘E code.

## Command line

The daemon owns every terminal, so the app, the terminal UI (`pitwall` on its own) and the CLI all drive the same sessions:

```sh
pitwall ls                                # tasks and their status
pitwall inbox                             # what's waiting on you
pitwall new fix-auth -p "fix the login redirect loop"   # an agent on its own branch
pitwall attach fix-auth                   # one session full screen in any terminal (Ctrl-] detaches)
pitwall approve 3 | deny 3                # answer a pending approval
pitwall snaps fix-auth                    # its undo history
pitwall diff fix-auth '#5^' '#5'          # what one step changed
pitwall revert fix-auth '#4'              # roll back to a step
pitwall hosts                             # your remotes, ~/.ssh/config hosts included
pitwall run tag:web -- 'journalctl -u nginx -n 50'   # policy-checked SSH across remotes
pitwall ask "why is prod-3 returning 502s?"          # an LLM investigation over SSH, saved as a runbook
pitwall stop                              # stop the daemon and everything it runs
```

![the terminal UI](docs/tui.png)

## Configure

**`~/.config/pitwall/config.toml`** holds every setting: the daemon's, the CLI's and both apps'. `pitwall init` writes it with every option documented (an older `~/.pitwall/config.toml` is moved here the first time pitwall runs). The daemon's sections look like this:

```toml
[agents.aider]
command = "aider"
prompt_mode = "type"        # "arg": prompt as the last argument · "type": typed in · "none"
attention_on_idle = true

[policy]                    # agents' shell commands and fleet commands
read_only = "allow"
mutating = "ask"
destructive = "deny"
rules = [{ pattern = "cargo test*", decision = "allow" }]

[hooks]                     # how agents' commands are gated
mode = "queue"              # "queue": approvals wait in pitwall · "agent": the agent's own prompt
auto_allow_read_only = false

[tui]
theme = "base16-ocean.dark" # the editor's syntax theme
```

Remotes added in the app are written to `hosts`, which has to sit above the first `[table]` (in TOML a key below one belongs to that table; older configs had it under `[ops]`, where it was never read, and entries there are still picked up):

```toml
hosts = [
  { name = "build box", addr = "192.168.50.226", user = "ubuntu", tags = ["lab"] },
  { name = "db-1", addr = "db.internal", port = 2222, key = "~/.ssh/id_ed25519" },
]
```

**Settings** (⌘, or ⚙ at the foot of the sidebar) is the app's half of the same file, written as `[app]` and `[keys]` at the end of it. Change things in either place: pitwall picks up a hand edit within a couple of seconds, and keeps your comments and the daemon's sections when it saves.

- font (any installed monospace font or a font file), weight, bold weight, size and line height; pick a Nerd Font if your prompt uses icons;
- one of 26 colour themes, cursor style, transparency and blur (a see-through window costs the system some GPU to blend what's behind it, so it has no shadow and turns opaque while another app is in front);
- your editor, the default agent and whether new agents get a worktree;
- tabs at the top or bottom;
- every shortcut (below);
- snippets: commands you run often, from the command bar or a terminal's right-click menu.

Without a chosen font pitwall uses SF Mono or Menlo (JetBrains Mono or DejaVu on Linux) with symbol, CJK and colour emoji fallbacks, or `PITWALL_FONT=/path/to/font.ttf`.

What isn't a setting stays in `~/.pitwall`: the layout (projects, tabs, cards and open files) in `layout.json`, put back on the next launch; the names you give projects, tabs and remotes, remote groups, which projects and groups are folded and the sidebar's tab in `state.json`; and your API keys, encrypted, in `apikeys.json`.

## How it works

```
pitwall-gui (egui + wgpu)    pitwall (terminal UI)    pitwall <cmd> (CLI)    Claude Code / Codex hooks
        └──────────────────────────┬─────────────────────┬────────────────────┘
                       ~/.pitwall/pitwall.sock (msgpack)
                                   │
            pitwall daemon: PTYs and vt100 screens, worktrees, shadow-git snapshots,
                     approvals, command policy, agent messages, audit log
```

The daemon owns the terminals; the app keeps a local copy of each screen, fed by the daemon's output, and draws it on the GPU. That's why closing the window never stops an agent.

```
crates/
  pitwall-core/   protocol, config, command policy, agent detection, git, editor, file tree, client
  pitwall/        the `pitwall` binary: daemon, CLI and terminal UI
  pitwall-gui/    the app: state and actions (app.rs), the UI (views/), the terminal view, tiling, settings
assets/          the icon: pitwall.svg (and a simpler pitwall-small.svg for 16 and 32 px); `make icon` rebuilds Pitwall.icns
scripts/         the app bundle's Info.plist, the Homebrew cask template, macos-app.sh
Makefile         build, install and publish (`make help`)
```

`cargo test --workspace` runs 102 tests: the command policy (including compound commands against allow rules), config merging, snapshots (dedupe, diff, revert, undoing a revert), git (hunks, commit, merge), the editor (selection, its Markdown keys and colouring), accepting an agent's work into a real worktree, Codex and Claude hook payloads, the file tree, terminal rendering and key encoding, colour emoji, card layout, agent messages, briefs and their steps, Markdown scroll sync, shortcuts, secret redaction, `~/.ssh/config` hosts, agents spotted in remote sessions, and API key encryption.

## Releasing

`make help` lists the steps. A release is the version in `Cargo.toml`, on a tag you push yourself:

```sh
make tools                               # once: Rust targets for both Mac architectures
git tag v0.2.0 && git push origin v0.2.0
make publish                             # test, build a universal Pitwall.app, release it on GitHub, update the cask
```

The app is signed ad hoc, and the cask clears the quarantine flag so Gatekeeper opens it. With a Developer ID, `make publish DEVELOPER_ID="Developer ID Application: …" NOTARY_PROFILE=pitwall` signs and notarizes it instead. `REPO` and `TAP` name the GitHub repo and the Homebrew tap (`jason-shen/pitwall` and `jason-shen/homebrew-tap` by default); `make check` says what's missing before anything is built or uploaded.

## Known limitations

- Terminals survive the app closing. When the daemon stops (`pitwall stop`, a reboot) their processes end; the next daemon starts them again in the same folders, with the same command (an editor card comes back as its editor), and agents that had finished a turn resume their session (`resume_args` in each agent's config); one that never got going starts over with its task. A shell comes back as a fresh shell. A task that can't start again (its folder is on a drive that isn't mounted, say) is kept for the next start rather than forgotten.
- The approval bar works with Claude Code and Codex. Other agents' own prompts show as *waiting* and are answered in their tab.
- Agents on a remote are spotted from their screen and aren't hooked into pitwall's policy, so their commands don't come through the approval bar.
- Remote sessions need an `ssh` binary on the machine running the daemon. Remote groups live in the app; the CLI selects remotes by name and tag.
- The built-in editor has no multi-cursor. With nothing selected, copy and cut take the whole line.
- Card sizes reset when the app restarts.

## License

MIT
