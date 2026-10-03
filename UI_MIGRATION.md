# Odysseus — UI Migration

**Source frontend:** 165 JS files / 135,402 LOC / 6.23 MB in `static/js/`, plus
`static/app.js` (4,583 L), `static/index.html` (2,591 L), `static/style.css`
(41,401 L / 1.29 MB), `static/lib/` (7.2 MB vendored UMD).
**Target:** Leptos + `leptos_router` + `leptos_meta` + Tailwind + source-owned
shadcn-style primitives.

---

## 1. Current frontend architecture (measured)

| Property | Finding |
|---|---|
| Framework | **None.** Native ES modules, no bundler, no transpiler, no minifier. |
| Build step | **None.** `package.json` has one devDependency (`bombadil`, a docs tool). |
| Routing | **None.** Boot-time `switch` on `location.pathname`; routes are synthetic DOM clicks (`app.js:1105-1234`). |
| Global state | **234 `window.*` writes.** 5 documented (`app.js:58-62`), ~229 more. |
| API access | Bare `globalThis.fetch` in every module. **No API client.** ~63 `/api/*` namespaces. |
| `API_BASE` | Re-declared in **~25 modules** with **3 coexisting conventions**. |
| Module instances | **`chatRenderer.js` loads 3×**; 11 other modules 2× each, due to mismatched manual `?v=` query strings. |
| Code splitting | One mechanism: `panels.js` memoising dynamic import. Only `galleryEditor.js` registered. |
| Styling | One 41K-line CSS file; theming via CSS custom properties. |
| First paint | Inline nonce'd script sets theme CSS vars from `localStorage` before paint. |

### 1.1 The `window.*` count is the real coupling measure

Line counts understate the coupling. The genuine figure is the 234 global writes.
The ones that must become signals/contexts or callbacks:

| Global | Writes | Nature |
|---|---|---|
| `window._suppressSidebarClose` | 14 | Re-entrancy guard |
| `window._persistentChatSession` | 10 | Chat session pin |
| `window.__odysseusDefaultChat` | 8 | Cached default model |
| `window.__galleryAllowCloseEditor` | 8 | Editor close guard |
| `window.syncRailSide` | 7 | Exported late-bound from `sidebar-layout.js:120,358` |
| `window._tasksFormEsc` | 6 | Escape guard |
| `window.__odysseusQueueStreamingSubmit` | 6 | Mobile queue |
| `window._odysseusKeybinds` | 6 | Keyboard map |
| `window._realContextLengths` | 4 | Model context cache |
| + ~150 singletons | 1–2 each | |

Named **function** globals used as cross-module callbacks (become `Callback`/`Context`):
`_syncResearchIndicator`, `_syncRagIndicator`, `_syncModelPickerAutohide`,
`_syncGroupIndicator`, `_syncRailDynamic`, `refreshChatContextHeader`,
`compactCurrentChatContext`, `__odysseusGetActiveEmailContext`,
`__odysseusSetChatMode`, `_updateSendBtnIcon`, `toggleSources`, `styledConfirm`,
`_restoreSidebarIfRouteCollapsed`, `_odyOpenSidebar`, `cancelActiveTour`.

**This is real coupling, not cargo cult** — `init.js:306,342` call
`typeof syncRailSide === 'function'` (global) *while also importing it from
`sidebar-layout.js`, and `sidebar-layout.js:5-13` deliberately defers its own export
until `initSidebarLayout` runs.

---

## 2. Target structure

```
odysseus-ui/
├── Cargo.toml                     # ssr + hydrate features
├── style/tailwind.css             # @tailwind directives + CSS variable theme layer
├── public/
│   ├── fonts/                     # Fira Code, Inter, OpenDyslexic (already self-hosted)
│   ├── icons/                     # PWA icons
│   └── sw.js                      # rewritten for hashed assets
└── src/
    ├── lib.rs                     # #[cfg(feature="hydrate")] mount, #[cfg(feature="ssr")] handlers
    ├── app.rs                     # <Routes/> composition
    ├── context.rs                 # AppContext — replaces window.* globals
    ├── api.rs                     # Typed API client — replaces bare fetch
    ├── error.rs
    ├── theme.rs                   # Theme definition + CSS variable writer
    │
    ├── components/
    │   ├── ui/                    # SOURCE-OWNED shadcn-style primitives
    │   │   ├── button.rs
    │   │   ├── input.rs
    │   │   ├── textarea.rs
    │   │   ├── dialog.rs
    │   │   ├── alert_dialog.rs
    │   │   ├── dropdown_menu.rs
    │   │   ├── popover.rs
    │   │   ├── command.rs
    │   │   ├── sidebar.rs
    │   │   ├── tabs.rs
    │   │   ├── card.rs
    │   │   ├── badge.rs
    │   │   ├── tooltip.rs
    │   │   ├── select.rs
    │   │   ├── checkbox.rs
    │   │   ├── switch.rs
    │   │   ├── progress.rs
    │   │   ├── scroll_area.rs
    │   │   ├── separator.rs
    │   │   ├── skeleton.rs
    │   │   ├── toast.rs
    │   │   ├── table.rs
    │   │   ├── accordion.rs
    │   │   ├── collapsible.rs
    │   │   ├── toggle_group.rs
    │   │   ├── slider.rs
    │   │   ├── label.rs
    │   │   ├── kbd.rs
    │   │   ├── code_block.rs
    │   │   └── mod.rs
    │   │
    │   ├── layout/                # Application-specific shell
    │   │   ├── shell.rs
    │   │   ├── sidebar.rs
    │   │   ├── sidebar_layout.rs
    │   │   ├── topbar.rs
    │   │   ├── modal.rs
    │   │   ├── tile.rs
    │   │   ├── prepaint.rs
    │   │   ├── loader.rs
    │   │   └── manifest.rs
    │   │
    │   ├── chat/                  # chat/
    │   │   ├── mod.rs
    │   │   ├── composer.rs
    │   │   ├── message.rs
    │   │   ├── message_list.rs
    │   │   ├── transcript.rs      # multi-round agent transcript reconstruction
    │   │   ├── stream_renderer.rs # frozen blocks + live tail + fence append-mode
    │   │   ├── segmenter.rs
    │   │   ├── thinking.rs
    │   │   ├── approval_card.rs
    │   │   ├── ask_user_card.rs
    │   │   ├── tool_event.rs
    │   │   ├── plan_update.rs
    │   │   ├── sources.rs
    │   │   ├── metrics.rs
    │   │   ├── attachments.rs
    │   │   ├── slash.rs
    │   │   ├── group.rs
    │   │   └── compare.rs
    │   │
    │   ├── sessions.rs            # sessions/
    │   ├── documents/
    │   │   ├── mod.rs
    │   │   ├── editor.rs
    │   │   ├── tabs.rs
    │   │   ├── library.rs
    │   │   ├── pdf_pane.rs
    │   │   ├── email_composer.rs
    │   │   ├── markdown.rs
    │   │   └── streaming_bridge.rs
    │   ├── email/                 # email/
    │   │   ├── mod.rs  inbox.rs  library.rs  reader.rs
    │   │   ├── composer.rs  scheduled.rs  unsubscribe.rs  library_utils.rs
    │   ├── research/              # research/
    │   │   ├── mod.rs  panel.rs  jobs.rs  synapse.rs
    │   ├── models.rs              # models/
    │   ├── settings/              # settings/
    │   │   ├── mod.rs  registry.rs  navigation.rs  sidebar.rs
    │   │   ├── search.rs  lifecycle.rs
    │   │   └── panels/{services,added_models,ai,search,integrations,
    │   │                email,reminders,appearance,shortcuts,
    │   │                account,tools,users,system}.rs
    │   ├── notes.rs               # notes/
    │   ├── tasks.rs               # tasks/
    │   ├── calendar.rs            # calendar/
    │   ├── memory.rs              # memory/
    │   ├── skills.rs              # skills/
    │   ├── gallery.rs             # gallery/
    │   ├── editor/                # editor/  (client-only, lazy)
    │   │   ├── mod.rs  state.rs  canvas.rs  layers.rs  history.rs
    │   │   ├── tools/{stroke,crop,flood_fill,lasso,move,wand,transform}.rs
    │   │   ├── fx/{pixel_pass,adj_popup,histogram}.rs
    │   │   ├── filters/{blur,edge_feather}.rs
    │   │   └── build/{controls,toolbar,topbar,right_panel,popups}.rs
    │   ├── cookbook.rs            # cookbook/
    │   ├── assistant.rs
    │   └── ui_visibility.rs
    │
    └── routes/                    # leptos_router
        ├── mod.rs
        ├── root.rs                # "/" — shell
        ├── sessions.rs            # "/sessions/:id"
        ├── notes.rs               # "/notes"
        ├── calendar.rs            # "/calendar"
        ├── email.rs               # "/email"
        ├── memory.rs              # "/memory"
        ├── gallery.rs             # "/gallery"
        ├── tasks.rs               # "/tasks"
        ├── cookbook.rs            # "/cookbook"
        ├── library.rs             # "/library"
        ├── documents.rs           # "/documents/:id"
        ├── compare.rs             # "/compare/:id"
        ├── research.rs            # "/research/:id"
        └── login.rs               # "/login"
```

### 2.1 `components/ui/` boundary rule

**`components/ui/` contains zero Odysseus-specific concepts.** A primitive may know
about variants, sizes, and focus rings. It may not know what a session, a model, a tool,
or an email is.

| Goes in `components/ui/` | Goes in `components/<domain>/` |
|---|---|
| `dialog.rs` — open/close, focus trap, portal, `Escape` | `approval_card.rs` — knows `PendingToolApproval` |
| `command.rs` — filtered list + keyboard nav | `slash.rs` — knows the 47-command registry |
| `sidebar.rs` — collapsible regions | `sidebar.rs` — knows the 3-state full/mini/off rail |
| `command.rs` | `model_picker.rs` — knows favorites/recent keys |
| `popover.rs` | `sources.rs` — knows `web_sources` payload |
| `toast.rs` | — but `showToast`'s swipe-dismiss is generic enough to move in |

---

## 3. File-by-file mapping

### 3.1 Shell & boot

| Current | Lines | Target | Notes |
|---|---|---|---|
| `static/index.html` | 2,591 | `app.rs` + `layout/*` | Becomes a component tree. |
| ↳ inline theme script `:17-206` | 190 | `layout/prepaint.rs` | **FOUC avoidance is non-negotiable.** Must run before first paint. Emits CSS custom properties + favicon + `theme-color` from `localStorage`. |
| ↳ loader wave `:212-235` | 24 | `layout/loader.rs` | |
| ↳ per-route manifest `:182-206` | 25 | `layout/manifest.rs` | |
| ↳ 34 script tags `:2556-2589` | 34 | **REMOVED** | Replaced by the bundler manifest. |
| `static/app.js` | 4,583 | `app.rs` + `context.rs` | |
| ↳ `_routeOpen` `:1105-1234` | 129 | `routes/*` | `leptos_router`. |
| ↳ `startOdysseusApp()` `:3662` | — | `app.rs` | Idempotency flag becomes a `OnceCell`. |
| ↳ 401 fetch interceptor `:196-204` | 9 | `api.rs` | |
| ↳ `deferRouteOpener` / `runDeferredRouteOpener` | — | `routes/mod.rs` | `ROUTES_NEEDING_SESSIONS` becomes route metadata. |
| `static/js/init.js` | 421 | `layout/*.rs` | 14 extracted IIFEs: auth-status gating, localStorage wipe on user switch, sidebar collapse default, CSS-var publishing, resizable sidebar, `visualViewport` keyboard handling, welcome-screen release. |
| `static/js/startupShell.js` | 153 | `app.rs` | Boot sequencing. |
| `static/js/appConfig.js` | 86 | `context.rs` | **The model to generalise.** Promise-slots + invalidation + `sessionStorage` prefetch handoff. |
| `static/js/platform.js` | — | `context.rs` | |
| `static/js/storage.js` | 125 | `storage.rs` | 25 named keys, all try/catch-guarded. |

### 3.2 Shell/layout

| Current | Lines | Target |
|---|---|---|
| `sidebar-layout.js` | 613 | `layout/sidebar_layout.rs` — 3-state signal + `--icon-rail-w`/`--sidebar-w` CSS vars via `ResizeObserver` |
| `ui.js` | 875 | `ui/{toast,dialog,scroll,dom}.rs` — **imported by 68 modules today** |
| ↳ global capture-phase Escape arbiter | — | `ui/dialog.rs` priority cascade |
| `modalManager.js` | 1,560 | `layout/modal.rs` — `_modalTopZ` from 300, chip dock, chain physics |
| `tileManager.js` | 16K B | `layout/tile.rs` |
| `modalSnap.js` | 47K B | `layout/tile.rs` |
| `toolWindowZOrder.js` | 2K B | `layout/tile.rs` — `topPortalZ()` computed from the live stack |
| `escMenuStack.js` | 4.5K B | `ui/dropdown_menu.rs` — DOM-free core preserved for testability |
| `theme.js` | 2,115 | `theme.rs` + `layout/prepaint.rs` |
| `panels.js` | 57 | Lazy-import registry → `leptos::Resource` / `Suspense` |
| `a11y.js` | 220 | `ui/a11y.rs` |
| `keyboard-shortcuts.js` | 480 | `ui/keyboard.rs` |
| `spinner.js` | 450 | `ui/progress.rs` |
| `censor.js` | 400 | `layout/censor.rs` |
| `tourAutoplay.js`, `tourHints.js` | 12K B | `ui/tour.rs` |
| `section-management.js` | 9.5K B | `layout/section.rs` |
| `windowDrag.js`, `windowResize.js` | 25K B | `layout/{drag,resize}.rs` |
| `ui_visibility.js` | 3.5K B | `ui_visibility.rs` |

### 3.3 Chat

| Current | Lines | Target | Notes |
|---|---|---|---|
| `chat.js` | 6,738 | `chat/` (7 modules) | `handleChatSubmit` alone is ~1,700 lines → `chat/composer.rs` + `chat/mod.rs`. |
| `chatRenderer.js` | 3,126 | `chat/transcript.rs` + `chat/{message,tool_event,sources,metrics,attachments}.rs` | 34 named exports. |
| ↳ `addMessage(role, content, modelName, metadata)` `:2561` | — | `chat/transcript.rs` | **Metadata is a first-class transcript format.** Rust struct with `tool_events[]` grouped by round, `round_texts[]`, `round_models[]`, `round_endpoint_ids[]`, `pending_ask_user`. |
| ↳ model colour/cost helpers | — | `chat/metrics.rs` | |
| `chatStream.js` | 327 | `chat/ui_control.rs` | **The LLM-manipulates-the-UI control plane.** |
| ↳ `handleUIControl` `:45-247` | 203 | `chat/ui_control.rs` | Verbs: `toggle`, `set_mode`, `switch_model`, `set_theme`, `create_theme`, `highlight`, `clear_highlight`, `research_started`, `open_panel`, `open_email_reply`. **Security surface**: arbitrary `querySelector` from model output. |
| `streamingRenderer.js` | 206 | `chat/stream_renderer.rs` | **The best-engineered module in the repo — port this pattern precisely.** |
| `streamingSegmenter.js` | ~250 | `chat/segmenter.rs` | Pure, fuzz-tested. |
| `liveThinkingThrottle.js` | ~250 | `chat/thinking.rs` | DI-first trailing-edge coalescer, 100 ms. |
| `chatStreamErrors.js` | 23 | `chat/mod.rs` | |
| `chatModelProvenance.js` | ~130 | `chat/metrics.rs` | |
| `slashCommands.js` | 6,520 | `chat/slash.rs` | **Declarative registry — the second-best-architected subsystem.** 47 top-level commands, `alias[]/category/help/usage/default/hidden/noUserBubble/handler`, `LEGACY_ALIASES` (~35), setup wizard, dynamic skill commands. |
| `slashAutocomplete.js` | 10.7K B | `chat/slash.rs` | |
| `composerArrowUpRecall.js` | 6.2K B | `chat/composer.rs` | |
| `codeRunner.js` | 14K B | `chat/message.rs` | `.run-code` button. |
| `tts-ai.js` | 700 | `chat/tts.rs` | Sentence-by-sentence streaming TTS. |
| `voiceRecorder.js` | 250 | `chat/composer.rs` | |

#### 3.3.1 `stream_renderer.rs` — port these five behaviours

1. **Frozen blocks** rendered once and never touched. Avoids code-block hover-button
   flicker and O(N²) re-highlighting.
2. **Live tail** re-renders per token, **except** an open ` ``` ` fence which uses
   **append-mode** — appending only new characters to a stable text node. No re-parse,
   no re-highlight.
3. **`fadeNewText`** walks text nodes and wraps only new characters past `prevLen` in a
   `.token-new` span, skipping `pre` and `.thinking-content`.
4. **Self-healing**: if something clobbers `innerHTML`, the tail marker is orphaned;
   `update()` detects `tailMarker.parentNode !== container` and rebuilds.
5. **Degradation latch**: any throw sets `degraded = true` and permanently falls back to
   full re-render — *"a bug can never produce broken output, only today's behaviour."*

Documented precondition: callers must pass **append-only** text.

### 3.4 Sessions

| Current | Lines | Target | Notes |
|---|---|---|---|
| `sessions.js` | 3,689 | `sessions.rs` + `routes/sessions.rs` | 27 exports + default namespace object. |
| ↳ history truncation `:22-25` | — | `sessions.rs` | `HISTORY_DISPLAY_CHAR_LIMIT = 160000`, `TAIL_CHARS = 20000`, paging 8 mobile / 24 desktop. |
| ↳ skeleton loader `:50-80` | 31 | `ui/skeleton.rs` | 8–16 fake bubbles sized to viewport height, alternating role, varied widths. Cheap to port, good UX. |
| ↳ 6 per-session stream Maps `:622-630` (in `chat.js`) | — | `context.rs` | Six `HashMap<String, …>` keyed by session id. |
| ↳ generation counter `:1938-1950` | — | `chat/mod.rs` | A superseded preflight send must bail **before** the POST. Subtle correctness logic — port explicitly. |
| ↳ streaming/research indicators `:2597-2781` | 185 | `sessions.rs` | Class toggles driven by background transitions. |
| ↳ incognito sessions | — | `sessions.rs` | `sessionStorage`, unlike everything else. |

### 3.5 Documents & editor

| Current | Lines | Target | Notes |
|---|---|---|---|
| `document.js` | 11,207 | `documents/` (7 modules) | **Largest file in the app. Contains four overlapping subsystems.** |
| ↳ tabbed editor `renderTabs:283` | — | `documents/tabs.rs` | |
| ↳ PDF annotation pane `:1149,1390,1781` | ~900 | `documents/pdf_pane.rs` | `{{annot:…}}` markdown syntax, undo snapshots. |
| ↳ email composer | ~700 | `documents/email_composer.rs` | RFC-3676 quote folding, HTML↔plaintext, localStorage drafts. |
| ↳ streaming bridge `:10309,10485` | — | `documents/streaming_bridge.rs` | Driven by `doc_stream_open`/`doc_stream_delta`. **Ordering matters** or the editor enters diff mode. |
| `documentLibrary.js` | 3,422 | `documents/library.rs` | Chats/Documents/Research/Archive tabs, `topPortalZ`. |
| `markdown.js` | 1,229 | `documents/markdown.rs` | Hand-rolled ordered regex pipeline — see §5. |
| `editor/` (56 files) | — | `editor/` (client-only, lazy) | `state.js` is the documented migration away from `let`. |
| `galleryEditor.js` | 4,386 | `editor/canvas.rs` + `editor/*` | |
| `gallery.js` | 3,006 | `gallery.rs` | |
| `fileHandler.js` | 465 | `chat/attachments.rs` | |
| `search-chat.js` | 180 | `chat/mod.rs` | Ctrl+K history search. |

### 3.6 Email, calendar, notes, tasks

| Current | Lines | Target |
|---|---|---|
| `emailLibrary.js` | 8,771 | `email/{library,reader,composer,scheduled,unsubscribe}.rs` — ~60 endpoints |
| `emailInbox.js` | 2,400 | `email/inbox.rs` |
| `emailLibrary/` (4 files) | — | `email/library_utils.rs`, `email/state.rs`, `email/reply_recipients.rs` |
| `emailShared.js` | 723 B | `api.rs` URL builders |
| `calendar.js` | 3,722 | `calendar.rs` — **holds most of the app's custom date/time pickers**, re-exported by `tasks.js` |
| `calendar/` (2 files) | — | `calendar/{utils,reminders}.rs` |
| `notes.js` | 5,365 | `notes.rs` — sidebar panel, not a modal |
| ↳ headless background agent `:4527` | — | `notes.rs` — POSTs `/api/chat_stream` with `mode=agent`, `disabled_tools=["manage_notes"]`, **drains the SSE reader to EOF without rendering** |
| `tasks.js` | 3,188 | `tasks.rs` — 30 s notification poll → replace with broadcast subscription |

### 3.7 Models, memory, skills, research, cookbook

| Current | Lines | Target |
|---|---|---|
| `modelPicker.js` | 39K B | `models.rs` — composer dropdown |
| `models.js` | 24K B | `models.rs` — sidebar section; shares `odysseus-model-favorites` with the picker |
| `modelSort.js`, `model/matchKey.js` | — | `models.rs` |
| `providers.js` | 17K B | `models/providers.rs` — pure `[regex, inline SVG]` table, ~25 providers, imported 18× |
| `providerDeviceFlow.js` | 4.1K B | `models/device_flow.rs` |
| `memory.js` | 1,550 | `memory.rs` — "Brain" modal |
| `skills.js` | 2,025 | `skills.rs` — tab inside the memory modal; only 2 exports today |
| `research/panel.js` | 61K B | `research/panel.rs` |
| `research/jobs.js` | 12.5K B | `research/jobs.rs` — **20 s poll → broadcast** |
| `researchSynapse.js` | 9.3K B | `research/synapse.rs` |
| `rag.js` | 177 | `memory.rs` — **the well-scoped reference module** |
| `cookbook.js` + 6 siblings | ~875K B | `cookbook/` (8 modules) — uses `/api/shell/exec` in 6 places, `/api/shell/stream` once; **cross-tab leader election via localStorage** must be preserved explicitly |
| `assistant.js` | 850 | `assistant.rs` |
| `presets.js` | 1,200 | `chat/composer.rs` + `settings/panels/services.rs` |
| `group.js` | 1,250 | `chat/group.rs` |
| `search.js` | 50 | `api.rs` |
| `settings.js` + `admin.js` | 8,856 | `settings/` (14 files) — see §4.1 |

### 3.8 Utility

| Current | Lines | Target |
|---|---|---|
| `langIcons.js` | 7.7K B | `ui/lang_icons.rs` |
| `emojiPicker.js`, `emojiShortcodes.js` | 28K B | `ui/{emoji_picker,shortcodes}.rs` |
| `colorPicker.js` | 16.6K B | `ui/color_picker.rs` |
| `color/` | — | `theme/` |
| `markdown/` | — | `documents/markdown.rs` |
| `util/` | — | `util/` |
| `modalSnap.js`, `toolWindowZOrder.js` | — | `layout/tile.rs` |

---

## 4. Patterns worth copying, not just porting

### 4.1 `settings/registry.js` — declarative metadata

The only subsystem with a real registry. `SETTINGS_GROUPS` (5 frozen groups) +
`SETTINGS_PANELS` (13 frozen panels), each with `{id, label, group, controller,
keywords}`, plus `getSettingsRegistryIssues()` which validates every registry panel has
a matching DOM node.

The dual-controller split (4 panels have `controller: 'admin'` and defer to
`admin.js`) is the natural Leptos seam: **13 components under one `SettingsModal`
shell**. In Rust this becomes a `SettingsPanel` trait with a `Vec<Box<dyn
SettingsPanel>>` registry, and the validation becomes a test.

### 4.2 `slashCommands.js` — declarative command tree

`COMMANDS` is a frozen object; 47 top-level commands with nested `subs`, per-command
`alias[]/category/help/usage/default/hidden/noUserBubble/handler`. Handlers receive
`(args, ctx)` where `ctx = {sid, esc}`.

Target: `Command { aliases: Vec<&str>, category, help, usage, subs, handler:
Box<dyn Fn(&[String], &SlashCtx) -> SlashOutcome> }`. Skills contribute dynamic
commands from `GET /api/skills/slash-catalog`, so the tree is server-extensible.

### 4.3 `editor/state.js` — documented migration away from `let`

> *"galleryEditor.js used to own ~110 module-scope `let` declarations and capture them
> via closure. Tool modules can't import a `let` binding's mutations across module
> boundaries, so we move the state into a single exported OBJECT whose properties are
> freely mutated by anyone holding a reference."*

In Rust this is `RwSignal<EditorState>` / a `Store`. The reasoning transfers directly.

### 4.4 `compare/state.js` — the closest thing to a store

One mutable object with ~35 fields mutated by 9 sibling modules, plus `reset()`. Target:
`RwSignal<CompareState>` with `Default`.

### 4.5 `appConfig.js` — shared config cache

Promise-slots for `settings` and `tools`; clears the slot on rejection so a boot blip
doesn't poison the session; `sessionStorage` prefetch handoff from the login page;
`invalidateSettings()` / `invalidateTools()`. Writers must call invalidate.

Target: `RwSignal<Option<Arc<Settings>>>` + a `OnceCell`-backed loader. **This is the
model for every shared-resource cache in the Rust UI.**

---

## 5. `markdown.js` — highest-risk UI port

Hand-rolled ordered regex pipeline with placeholder substitution. No `marked`, no
`markdown-it`. Order matters and is load-bearing:

1. Extract fenced code blocks → placeholders. ` ```mermaid ` handled specially.
2. Extract **inline code — before link/autolink passes**, because a bare URL inside
   backticks was being corrupted.
3. **Repair agent-mangled entity anchors** — three regexes upgrading `[Name] [#kind-id]`,
   bare `[#kind-id]`, and bare `#kind-id` into markdown links against
   `ANCHOR_KIND = (session|document|note|image|email|event|task|skill|research)`.
   **This is the deep-link contract.**
4. Images → links → autolinks → tables/lists/headings → restore placeholders.

**Target:** `pulldown-cmark` for the base parse, plus **hand-written passes for steps
1–3**, because those encode behaviour `pulldown-cmark` will not replicate. Step 3 must
be preserved exactly or every agent-emitted `#note-123` link breaks.

**Security:** `sanitizeAllowedHtml` uses inert `<template>` parsing — **this is the
security boundary** for rendered content. Target: `ammonia` allowlist. Test vectors
from `test_markdown_dom_xss_helpers.py` and `test_prompt_injection_audit.py` are
mandatory.

Other exports: `processWithThinking`, `normalizeThinkingMarkup`,
`extractThinkingBlocks`, `hasUnclosedThinkTag`, `startsWithReasoningPrefix`,
`createCollapsible`, `svgifyEmoji`, `renderMermaid`, `renderMath`, `ensureMermaid`/
`ensureKaTex` (lazy lib injection, which saved ~985 KB/page).

---

## 6. SSR / hydration per surface

See `REWRITE_PLAN.md` §6 for the full table. Summary:

| Surface | SSR | Hydrate | Bundle |
|---|---|---|---|
| Shell, sidebar, nav | Yes | Yes | main |
| Session list, chat history | Yes (real DB data) | Yes | main |
| Document text | Yes | Yes | main |
| Settings shell + panel metadata | Yes | Yes | main |
| Theme | Inline critical CSS | Yes | main |
| Markdown/Mermaid/KaTeX | No | Yes | main (lazy) |
| Gallery/canvas editor | **No** | Lazy | separate |
| Calendar grid | **No** | Yes | main |
| Email grid/reader/composer | **No** | Yes | main |
| Compare panes | **No** | Yes | main |

### 6.1 The CSS-variable refactor is Phase 0 scope

Current CSP: `script-src 'self' 'nonce-…'`, `style-src` retains `'unsafe-inline'` with
an explicit comment that migrating requires auditing every JS-set `style=` attribute.

Leptos emits `style:` attributes for dynamic styling. Under a nonce-only `style-src`
this breaks. Options, in order of preference:

1. **CSS custom properties** — set `style="--w: 12rem"` (custom properties are *not*
   subject to `style-src` in practice under a nonce policy when the value is a
   var-assignment)… actually this is unreliable. Prefer:
2. **Class + data attributes** — `class="w-48"` or `data-width="lg"`, styled in CSS.
   Zero inline styles. This is the correct answer and also matches Tailwind's model.
3. Nonce'd `<style>` blocks emitted by Leptos for genuinely dynamic values.

**Decision:** option 2 as the default; option 3 only where a runtime-computed value is
unavoidable. The audit of every `style=` write is a Phase 0 task.

---

## 7. State management mapping

| Current | Target | Notes |
|---|---|---|
| 234 `window.*` globals | `AppContext` + signals | Enumerate each during Phases 4–5. |
| 6 per-session stream `Map`s in `chat.js` | `RwSignal<HashMap<SessionId, StreamState>>` | |
| `compare/state.js` mutable object | `RwSignal<CompareState>` + `Default` | |
| `editor/state.js` mutable object | `RwSignal<EditorState>` + `Default` | |
| `odysseus-toggles` localStorage blob | `RwSignal<Toggles>` persisted to localStorage | All chat-mode toggles live in one blob today. |
| 25 named `storage.js` keys | `storage.rs` typed accessors | Same key strings for migration continuity. |
| `sessionStorage` incognito set | Same | Distinct storage area — preserve. |
| `appConfig.js` promise-slots | `OnceCell` + `RwSignal` | |
| `localStorage` theme | `theme.rs` + prepaint script | |
| Cookie session | Server-set cookie; `AppContext` reads `/api/auth/status` | |

### 7.1 localStorage keys preserved

Compatibility requires these strings to survive, or users lose preferences:
`odysseus-theme`, `odysseus-ui-scale`, `odysseus-sidebar-mode`, `odysseus-toggles`,
`odysseus-models-collapsed`, `odysseus-model-favorites`, `odysseus-model-usage`,
`odysseus-model-sort`, `odysseus-model-recent`, `odysseus-session-date-section-collapsed`,
`odysseus-research-dismissed`, `odysseus-sensitive-blur`, `odysseus-cookbook-bg-leader`,
`cal-week-start`, `cal-wk-hour-px`, `cal-filters-collapsed`, `cal-notif-fired`,
`odysseus-calendar-badge-seen`, `odysseus-calendar-cache`, `ody-prefetch-settings`.

`localStorage` uses try/catch on every access today (private-browsing safety) — the Rust
wrapper must keep that.

---

## 8. Styling: Tailwind + shadcn-style

| Current | Target |
|---|---|
| `static/style.css` 41,401 lines | Tailwind utilities + `components/ui/**` primitives |
| CSS custom properties (`--bg`, `--fg`, `--panel`, `--border`, `--red`, `--brand-color`, 11 `--hl-*`, `--font-family`, density classes) | Tailwind theme extension in `style/tailwind.css`, driven by `:root[data-theme]` |
| `applyColors()` writes to `documentElement.style` | Theme signal writes `data-theme` attribute; CSS owns the values |
| 17 built-in themes + max 8 custom | `theme.rs` registry + `data-theme` |
| 4 fonts, UI density, UI text scale | Tailwind `font-*`, `density-*`, `ui-scale-*` |
| 12 animated background patterns | `layout/background.rs` |

**Theme values must round-trip.** `applyColors` derives 11 syntax-highlight variables
from the theme's HSL. Port the derivation exactly (the `h2hsl`/`hsl2hsl` pair in
`index.html:39-59`) or syntax highlighting visually regresses.

---

## 9. Frontend feature parity matrix

| Feature | Status | Phase | Notes |
|---|---|---|---|
| App shell | UNIMPLEMENTED | 0 | |
| Client routing | REPLACED | 0 | `leptos_router` replaces DOM-click routing |
| Sidebar/rail | UNIMPLEMENTED | 0 | |
| Modal manager | UNIMPLEMENTED | 0 | |
| Window tiling | UNIMPLEMENTED | 22 | |
| Escape arbiter | UNIMPLEMENTED | 0 | |
| Toasts / dialogs | UNIMPLEMENTED | 0 | |
| Theme + FOUC avoidance | UNIMPLEMENTED | 0 | Non-negotiable |
| PWA + SW | UNIMPLEMENTED | 0 | |
| Chat streaming UI | UNIMPLEMENTED | 4 | |
| Agent transcript | UNIMPLEMENTED | 4 | |
| Approval cards | UNIMPLEMENTED | 6 | |
| Slash commands | UNIMPLEMENTED | 14 | |
| Sessions | UNIMPLEMENTED | 1 | |
| Document editor | UNIMPLEMENTED | 8 | |
| Markdown renderer | UNIMPLEMENTED | 8 | |
| Email UI | UNIMPLEMENTED | 15 | Blocked by 15 |
| Calendar UI | UNIMPLEMENTED | 16 | Blocked by 16 |
| Notes | UNIMPLEMENTED | 12 | |
| Tasks | UNIMPLEMENTED | 13 | |
| Memory | UNIMPLEMENTED | 7 | |
| Skills | UNIMPLEMENTED | 14 | |
| Gallery | UNIMPLEMENTED | 17 | |
| Image editor | UNIMPLEMENTED | 17 | |
| Research panel | UNIMPLEMENTED | 11 | |
| Cookbook | UNIMPLEMENTED | 18 | |
| Settings (13 panels) | UNIMPLEMENTED | 20 | |
| **Local denoise/upscale/face-enhance UI** | **REMOVED** | — | Controls render disabled with a documented reason |
| Colour picker | UNIMPLEMENTED | 22 | |
| Emoji picker | UNIMPLEMENTED | 22 | |
| Code runner | UNIMPLEMENTED | 8 | |
| Keyboard shortcuts | UNIMPLEMENTED | 20 | |
| Accessibility | UNIMPLEMENTED | 0 | |
| Onboarding tours | UNIMPLEMENTED | 22 | |

---

## 10. Bundling and asset strategy

| Concern | Current | Target |
|---|---|---|
| Asset identity | Manual `?v=YYYYMMDDlabel` per importer | Content hash in the filename |
| Double-load bug | **Occurred in production**; 13 modules currently duplicated | Structurally impossible |
| Cache headers | `Cache-Control: no-cache` forced on js/css/html | `immutable` + hashed filenames |
| Code splitting | `panels.js` memoising dynamic import | Bundler chunks + `Suspense`/`Resource` |
| Mermaid (3.5 MB) | Lazy-injected, explicitly not precached | Same |
| KaTeX (~985 KB) | Lazy-injected | Same |
| Fonts | Self-hosted woff2 | Same |
| Vendored libs | 7.2 MB UMD in `static/lib/` | Bundled deps; drop what's unused |

**No Node.js in deployment.** Tailwind and the Leptos bundle are build-time only; the
shipped artifact is a single static binary plus hashed assets.