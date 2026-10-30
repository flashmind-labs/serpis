-- A chat on Claude Code (the stand-in claude, serpis-fake-claude): its permission prompts wait
-- on the tool's card and in a bar above the composer, answered with keys (C-c C-y) or clicks
-- (Deny, Always), and Always hands Claude Code its own suggestion back.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end
serpis.var.set("remote-rayfish", false)

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 200 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
local function has(text) return (serpis.chat.transcript() or ""):find(text, 1, true) ~= nil end
local function asking() return d.composer()["allow"] ~= nil end
local function idle_with(text) return function() return has(text) and not serpis.chat.running() and not asking() end end
local function click(name)
  local r = d.composer()[name]
  if not r then return end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end
local function ask(text, go)
  d.text(text)
  T.press("RET")
  when(asking, go)
  return false
end

local steps = {
  function(go)
    -- Its models come from the stand-in's handshake.
    when(function() return serpis.agent.find_model("claude-code/fake") ~= nil end, go)
    return false
  end,
  function(go)
    T.check("Claude Code's own models are offered", serpis.agent.find_model("claude-code/fake") ~= nil, true)
    serpis.command.run("new-chat")
    when(function() return d.composer()["send"] ~= nil end, go)
    return false
  end,
  function(go)
    -- Picked for the chat as any model is: the chat's machine lists it.
    T.press("C-c C-m")
    when(function() return d.minibuffer() == "Model: " end, function()
      d.text("claude-code/fake")
      serpis.defer(500, function()
        T.press("RET")
        when(function() return (serpis.chat.info() or {}).model == "claude-code/fake" end, go)
      end)
    end)
    return false
  end,
  function(go)
    T.check("chat-select-model offers it and the chat takes it", (serpis.chat.info() or {}).model, "claude-code/fake")
    return true
  end,
  function(go)
    -- Notified of what waits for you, whether looking or not (notify-backend is "none" here).
    serpis.var.set("notify-when", "always")
    return ask("please edit", go)
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      -- A moment later: a screenshot right away can show an earlier frame.
      serpis.defer(1500, function() d.screenshot(shots .. "/claude-code-ask.png"); serpis.defer(500, go) end)
      return false
    end
  end,
  function(go)
    T.check("the edit waits for you", asking(), true)
    T.check("the sidebar says it waits for you", (serpis.buffer.activity() or {}).state, "waiting")
    local n = (d.notifications() or {})[#(d.notifications() or {})] or {}
    T.check("and a notification says what", (n.event or "") .. " · " .. (n.body or ""), "waiting · Waiting for you: allow edit notes.txt")
    T.check("its card says so", has("Allow it?"), true)
    T.check("and shows the change it asks for", has("+ TWO"), true)
    T.check("the composer offers always", d.composer()["always"] ~= nil, true)
    T.press("C-c C-y")
    when(idle_with("Edited."), go)
    return false
  end,
  function(go)
    T.check("C-c C-y allowed it", has("Edited."), true)
    T.check("the bar is gone", asking(), false)
    return ask("edit no", go)
  end,
  function(go)
    click("deny")
    when(idle_with("Not allowed."), go)
    return false
  end,
  function(go)
    T.check("Deny refused it", has("Not allowed."), true)
    return ask("edit always", go)
  end,
  function(go)
    click("always")
    when(function() return idle_with("Edited.")() and select(2, (serpis.chat.transcript() or ""):gsub("Edited%.", "")) >= 2 end, go)
    return false
  end,
  function(go)
    local function mode() return (serpis.chat.info() or {}).permission_mode end
    -- Always on an edit switched Claude Code to accepting edits; the chat follows it.
    T.check("the chat shows the mode Claude Code switched to", mode(), "acceptEdits")
    T.check("its composer shows the mode", d.composer()["permission-mode"] ~= nil, true)
    -- Shift+Tab cycles the modes, as in Claude Code.
    T.press("S-TAB")
    when(function() return mode() == "plan" end, function()
      T.check("S-TAB: plan", mode(), "plan")
      T.press("S-TAB")
      when(function() return mode() == "auto" end, function()
        T.check("S-TAB: auto", mode(), "auto")
        T.press("S-TAB")
        when(function() return mode() == "default" end, go)
      end)
    end)
    return false
  end,
  function(go)
    local function mode() return (serpis.chat.info() or {}).permission_mode end
    T.check("S-TAB: back to default", mode(), "default")
    -- A click on it picks one.
    click("permission-mode")
    when(function() return d.minibuffer() == "Permission mode: " end, function()
      d.text("auto")
      serpis.defer(300, function()
        T.press("RET")
        when(function() return mode() == "auto" end, go)
      end)
    end)
    return false
  end,
  function()
    T.check("a click picks one", (serpis.chat.info() or {}).permission_mode, "auto")
    local log = sh("cat '" .. os.getenv("FAKE_CLAUDE_LOG") .. "'")
    T.check("and Claude Code is switched at once", log:find('"mode":"auto","subtype":"set_permission_mode"', 1, true) ~= nil or log:find('"subtype":"set_permission_mode","mode":"auto"', 1, true) ~= nil, true)
  end,
  function(go) return ask("a question", go) end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(1500, function() d.screenshot(shots .. "/claude-code-question.png"); serpis.defer(500, go) end)
      return false
    end
  end,
  function(go)
    -- Its questions wait on their card; C-c C-y asks them one by one in the minibuffer.
    T.check("the questions show on their card", has("Color · Which color?") and has("· Red — warm") and has("Sizes · Which sizes?  (any of)"), true)
    T.check("which says how to answer", has("Answer it? C-c C-y answer"), true)
    T.check("no always for questions", d.composer()["always"], nil)
    T.press("C-c C-y")
    when(function() return (d.minibuffer() or ""):find("Which color?", 1, true) ~= nil end, go)
    return false
  end,
  function(go)
    T.check("the first question is asked", (d.minibuffer() or ""):find("(1/2) Color · Which color?", 1, true) ~= nil, true)
    d.text("Blue")
    serpis.defer(200, function()
      T.press("RET")
      when(function() return (d.minibuffer() or ""):find("Which sizes?", 1, true) ~= nil end, go)
    end)
    return false
  end,
  function(go)
    -- Multiple choice: picks until Done.
    d.text("S")
    serpis.defer(200, function()
      T.press("RET")
      when(function() return (d.minibuffer() or ""):find("[S]", 1, true) ~= nil end, function()
        d.text("L")
        serpis.defer(200, function()
          T.press("RET")
          when(function() return (d.minibuffer() or ""):find("[S, L]", 1, true) ~= nil end, function()
            d.text("Done")
            serpis.defer(200, function()
              T.press("RET")
              when(idle_with("You chose Blue in S, L."), go)
            end)
          end)
        end)
      end)
    end)
    return false
  end,
  function(go)
    T.check("the answers reached it", has("You chose Blue in S, L."), true)
    T.check("and its card shows them", has('"Which color?"="Blue"'), true)
    return ask("another question", go)
  end,
  function(go)
    -- An answer of your own: words that match no option.
    T.press("C-c C-y")
    when(function() return (d.minibuffer() or ""):find("Which color?", 1, true) ~= nil end, function()
      d.text("Green please")
      serpis.defer(200, function()
        T.press("RET")
        when(function() return (d.minibuffer() or ""):find("Which sizes?", 1, true) ~= nil end, function()
          d.text("M")
          serpis.defer(200, function()
            T.press("RET")
            when(function() return (d.minibuffer() or ""):find("[M]", 1, true) ~= nil end, function()
              d.text("Done")
              serpis.defer(200, function()
                T.press("RET")
                when(idle_with("You chose Green please in M."), go)
              end)
            end)
          end)
        end)
      end)
    end)
    return false
  end,
  function(go)
    T.check("your own words are an answer", has("You chose Green please in M."), true)
    return ask("question to skip", go)
  end,
  function(go)
    click("deny")
    when(idle_with("No answers."), go)
    return false
  end,
  function(go)
    T.check("Skip declines them", has("No answers."), true)
    return ask("question for a script", go)
  end,
  function(go)
    -- Scripts answer with the answers as the param.
    serpis.command.run("chat-answer-questions", { ["Which color?"] = "Red", ["Which sizes?"] = "L" })
    when(idle_with("You chose Red in L."), go)
    return false
  end,
  function(go)
    T.check("chat-answer-questions takes answers from a script", has("You chose Red in L."), true)
    -- A subagent: its steps show in its card.
    d.text("use a subagent")
    T.press("RET")
    when(idle_with("The subagent said from-sub."), go)
    return false
  end,
  function(go)
    T.check("the subagent's command shows in its card", has("✓ bash  echo from-sub"), true)
    T.check("and what it said", has("It printed from-sub."), true)
    T.check("and how it went", has("1 tool · 1.2k tokens"), true)
    -- A command left running: the composer says so, and C-c C-k stops it.
    d.text("background forever")
    T.press("RET")
    when(function() return idle_with("Started it in the background.")() and d.composer()["background"] ~= nil and has("line 3") end, go)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(1000, function() d.screenshot(shots .. "/claude-code-background.png"); serpis.defer(500, go) end)
      return false
    end
  end,
  function(go)
    T.check("the composer shows what runs in the background", d.composer()["background"] ~= nil, true)
    T.check("its card says it runs on, and how much it printed", has("Running in the background · ") and has(" printed · C-c C-k stops it"), true)
    T.check("and shows what it prints, live", has("line 3") and has("progress 100%"), true)
    T.check("as plain text", has("\27["), false)
    -- A click on what runs shows it (its card, open, the cursor on it), and doesn't stop it.
    click("background")
    serpis.defer(700, function()
      T.check("a click on the background work shows it, and doesn't stop it", (serpis.chat.info() or {}).background, 1)
      local line = serpis.buffer.line((serpis.buffer.cursor())) or ""
      T.check("the cursor is on its card", line:find("sleep", 1, true) ~= nil, true)
      T.press("C-c C-k")
      when(function() return d.composer()["background"] == nil and has("Stopped") end, go)
    end)
    return false
  end,
  function(go)
    T.check("C-c C-k stopped it", has("Stopped"), true)
    -- Several at once: a shell and a monitor.
    d.text("background forever too")
    T.press("RET")
    when(idle_with("Started it in the background."), function()
      d.text("start a monitor")
      T.press("RET")
      when(function() return idle_with("Watching the log.")() and (serpis.chat.info() or {}).background == 2 end, go)
    end)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(1000, function() d.screenshot(shots .. "/claude-code-shell-and-monitor.png"); serpis.defer(500, go) end)
      return false
    end
  end,
  function(go)
    T.check("both run in the background", (serpis.chat.info() or {}).background, 2)
    T.check("the monitor's card says what it watches", has("Monitor  watch the log"), true)
    -- With two, C-c C-k asks which.
    T.press("C-c C-k")
    when(function() return d.minibuffer() == "Stop: " end, function()
      d.text("watch the log")
      serpis.defer(300, function()
        T.press("RET")
        when(function() return (serpis.chat.info() or {}).background == 1 end, go)
      end)
    end)
    return false
  end,
  function(go)
    T.check("C-c C-k asked which and stopped the monitor", (serpis.chat.info() or {}).background, 1)
    T.press("C-c C-k")
    when(function() return (serpis.chat.info() or {}).background == 0 end, go)
    return false
  end,
  function(go)
    T.check("with one left, C-c C-k stops it", (serpis.chat.info() or {}).background, 0)
    -- One that finishes after the turn: Claude Code takes a turn of its own, shown as any.
    d.text("run it in the background")
    T.press("RET")
    when(function() return has("The background command finished: bg-done.") and not serpis.chat.running() end, go)
    return false
  end,
  function()
    T.check("its own turn shows, after what started it", has("Background command finished: sleep then echo bg-done"), true)
    T.check("with what it said", has("The background command finished: bg-done."), true)
    T.check("and its card says how it ended", has("completed (exit code 0)"), true)
  end,
  function(go)
    -- Thinking that isn't sent still shows: a Thinking… line, with how much so far.
    serpis.var.set("claude-code-thinking-display", "omitted")
    serpis.defer(300, function()
      d.text("think it over")
      T.press("RET")
      when(function() return has("Thinking… · ~") end, go)
    end)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      d.screenshot(shots .. "/claude-code-thinking.png")
    end
    T.check("a reply that is only thinking says so, with its tokens", has("Thinking… · ~"), true)
    when(idle_with("Thought it through."), go)
    return false
  end,
  function(go)
    T.check("and goes once it answers", has("Thinking… · ~"), false)
    -- Summarized (the default), its thinking shows as any chat's does.
    serpis.var.set("claude-code-thinking-display", "summarized")
    serpis.defer(300, function()
      d.text("think again")
      T.press("RET")
      when(function() return idle_with("Thought it through.")() and has("Weighing it up.") end, go)
    end)
    return false
  end,
  function(go)
    T.check("summarized thinking shows", has("Weighing it up."), true)
    -- A plan waits for approval, in its own words.
    serpis.command.run("chat-select-permission-mode", "plan")
    serpis.defer(300, function() ask("make a plan", go) end)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(1000, function() d.screenshot(shots .. "/claude-code-plan.png"); serpis.defer(500, go) end)
      return false
    end
  end,
  function(go)
    T.check("the plan shows on its card", has("1. Create hello.txt"), true)
    T.check("which asks to approve it", has("Approve this plan?"), true)
    T.check("the bar offers accepting edits", d.composer()["always"] ~= nil, true)
    -- Keep planning: say what should change.
    click("deny")
    when(function() return d.minibuffer() == "What should change? " end, function()
      d.text("use hello.md")
      serpis.defer(300, function()
        T.press("RET")
        when(function() return has("Heard: The user wants to keep planning. What should change: use hello.md") and not serpis.chat.running() end, go)
      end)
    end)
    return false
  end,
  function(go)
    T.check("keep planning says what should change", has("What should change: use hello.md"), true)
    serpis.command.run("chat-select-permission-mode", "plan")
    serpis.defer(300, function() ask("make a plan again", go) end)
    return false
  end,
  function(go)
    T.press("C-c C-a")
    when(idle_with("Plan approved; on in acceptEdits."), go)
    return false
  end,
  function(go)
    T.check("C-c C-a approves it, accepting edits", has("Plan approved; on in acceptEdits."), true)
    T.check("and the mode it goes on in shows", (serpis.chat.info() or {}).permission_mode, "acceptEdits")
    -- A message typed while it works goes to it at once, in the same turn.
    d.text("a long task")
    T.press("RET")
    when(function() return has("sleep 1; echo long-done") end, go)
    return false
  end,
  function(go)
    d.text("use tabs")
    T.press("RET")
    serpis.defer(150, function()
      T.check("it shows as sent while it works", has("Queued · steers the running agent"), true)
      when(idle_with("Done. You said: use tabs."), go)
    end)
    return false
  end,
  function()
    T.check("it was taken in, in the same turn", has("Done. You said: use tabs."), true)
  end,
  function()
    local log = sh("cat '" .. os.getenv("FAKE_CLAUDE_LOG") .. "'")
    T.check("Always passed Claude Code's suggestion back", log:find('"updatedPermissions":%[{"destination":"session","mode":"acceptEdits"') ~= nil, true)
    T.check("the chat's session is Claude Code's", log:find('"--session-id"', 1, true) ~= nil, true)
  end,
  function(go)
    -- Close to a usage limit: the composer says how close.
    d.text("near the limit")
    T.press("RET")
    when(function() return idle_with("echo: near the limit")() and d.composer()["limit"] ~= nil end, go)
    return false
  end,
  function()
    local l = (serpis.chat.info() or {}).limit or {}
    T.check("the composer warns of the limit", d.composer()["limit"] ~= nil, true)
    T.check("chat.info says which and how full", (l.status or "") .. " " .. (l.kind or "") .. " " .. tostring(l.utilization), "allowed_warning five_hour 0.85")
  end,
  function(go)
    -- At it: a bar says until when, and that the chat goes on by itself then.
    serpis.var.set("claude-code-limit-continue-delay", 0)
    d.text("hit the limit for an hour")
    T.press("RET")
    when(function() return not serpis.chat.running() and d.composer()["limit-wait"] ~= nil end, go)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(1000, function() d.screenshot(shots .. "/claude-code-limit.png"); serpis.defer(500, go) end)
      return false
    end
  end,
  function(go)
    local info = serpis.chat.info() or {}
    T.check("stopped at the limit", (info.limit or {}).status, "rejected")
    T.check("it goes on by itself in an hour", info.continues_at ~= nil and info.continues_at > os.time() + 3500, true)
    T.check("the warning gives way to the bar", d.composer()["limit"], nil)
    local a = serpis.buffer.activity() or {}
    T.check("the sidebar says it stopped at the limit, not that it failed", a.state, "idle")
    T.check("and when it goes on", (a.detail or ""):match("^Session limit · goes on in [%dhm ]+$") ~= nil, true)
    -- Told to wait instead, it does; told again, it goes on at the reset.
    click("limit-wait")
    when(function() return (serpis.chat.info() or {}).continues_at == nil and d.composer()["limit-go"] ~= nil end, go)
    return false
  end,
  function(go)
    T.check("Don't go on makes it wait for you", (serpis.chat.info() or {}).continues_at, nil)
    T.check("as the sidebar says", ((serpis.buffer.activity() or {}).detail or ""):match("^Session limit · resets in %d+m · waits for you$") ~= nil, true)
    serpis.command.run("chat-toggle-continue-after-limit")
    when(function() return (serpis.chat.info() or {}).continues_at ~= nil and d.composer()["limit-wait"] ~= nil end, go)
    return false
  end,
  function(go)
    T.check("chat-toggle-continue-after-limit has it go on again", (serpis.chat.info() or {}).continues_at ~= nil, true)
    -- A limit that resets in moments: the chat goes on by itself.
    d.text("hit the limit")
    T.press("RET")
    when(function() return idle_with("echo: Your usage limit has reset. Continue where you left off.")() and d.composer()["limit-wait"] == nil end, go)
    return false
  end,
  function()
    T.check("it went on after the reset", has("echo: Your usage limit has reset. Continue where you left off."), true)
    T.check("and the bar is gone", d.composer()["limit-bar"], nil)
    T.check("as is the limit", (serpis.chat.info() or {}).limit, nil)
  end,
  function(go)
    -- A command it waits on: its card says C-c C-b moves it to the background, which does.
    d.text("long command")
    T.press("RET")
    when(function() return has("C-c C-b moves it to the background") end, go)
    return false
  end,
  function(go)
    T.check("the running command's card offers to move it", has("Running · ") or has("C-c C-b moves it to the background"), true)
    T.press("C-c C-b")
    when(function() return idle_with("Moved it to the background; going on.")() and (serpis.chat.info() or {}).background == 1 end, go)
    return false
  end,
  function(go)
    T.check("C-c C-b moved it, and the turn went on", has("Moved it to the background; going on."), true)
    T.check("it runs on in the background", (serpis.chat.info() or {}).background, 1)
    T.press("C-c C-k")
    when(function() return (serpis.chat.info() or {}).background == 0 end, go)
    return false
  end,
  function(go)
    -- How full its context is, as Claude Code counts it; compacting by itself, noted where it did.
    d.text("compact yourself")
    T.press("RET")
    when(function() return idle_with("echo: compact yourself")() and has("Compacted by itself") end, go)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(800, function() d.screenshot(shots .. "/claude-code-context.png"); serpis.defer(300, go) end)
      return false
    end
  end,
  function()
    T.check("its compaction shows where it happened", has("Compacted by itself: ") and has(" → 21.0k tokens"), true)
    T.check("the composer shows how full its context is", d.composer()["context"] ~= nil, true)
  end,
  function(go)
    -- Its /usage: the stand-in's made-up plan, asked through M-x.
    T.press("M-x")
    when(function() return d.minibuffer() ~= nil end, function()
      d.text("claude-code-usage")
      serpis.defer(300, function()
        T.press("RET")
        when(function() return serpis.buffer.name(serpis.buffer.current()) == "*claude-code-usage*" end, go)
      end)
    end)
    return false
  end,
  function(go)
    when(function() return (serpis.buffer.text(serpis.buffer.current()) or ""):find("Max plan", 1, true) ~= nil end, function()
      local shots = os.getenv("SERPIS_TEST_SHOTS")
      if not shots then return go() end
      serpis.defer(1000, function() d.screenshot(shots .. "/claude-code-usage.png"); serpis.defer(500, go) end)
    end)
    return false
  end,
  function(go)
    local cur = serpis.buffer.current()
    local page = serpis.buffer.text(cur) or ""
    T.check("claude-code-usage shows the usage page", serpis.buffer.name(cur), "*claude-code-usage*")
    T.check("with the plan", page:find("Claude Code usage · Max plan", 1, true) ~= nil, true)
    T.check("the session window", page:find("Current session", 1, true) ~= nil and page:find("42%", 1, true) ~= nil, true)
    T.check("a model's week", page:find("This week, Fake", 1, true) ~= nil, true)
    T.check("extra usage", page:find("Extra usage: on, 25% of its monthly limit used.", 1, true) ~= nil, true)
    T.check("and the advice", page:find("Start fresh when you switch tasks (90% of it): run /clear", 1, true) ~= nil, true)
    local function asked() return select(2, sh("cat '" .. os.getenv("FAKE_CLAUDE_LOG") .. "'"):gsub('"subtype":"get_usage"', "")) end
    local before = asked()
    -- g asks again, in place.
    T.press("g")
    when(function() return asked() > before and (serpis.buffer.text(cur) or ""):find("Max plan", 1, true) ~= nil end, function()
      T.check("g asks Claude Code again", asked(), before + 1)
      T.check("in the same page", serpis.buffer.name(serpis.buffer.current()), "*claude-code-usage*")
      T.press("q")
      serpis.defer(300, function()
        T.check("q closes it", serpis.buffer.name(serpis.buffer.current()) ~= "*claude-code-usage*", true)
        go()
      end)
    end)
    return false
  end,
  function(go)
    -- Scripts get the same answer.
    serpis.agent.claude_code_usage(function(u, err)
      T.check("serpis.agent.claude_code_usage answers", u and u.rate_limits.five_hour.utilization, 42)
      T.check("without the advice when asked so", u and u.behaviors, nil)
      T.check("and no error", err, nil)
      go()
    end, { advice = false })
    return false
  end,
  function()
    local log = sh("cat '" .. os.getenv("FAKE_CLAUDE_LOG") .. "'")
    T.check("advice = false skipped the transcript scan", log:find('"skip_behaviors":true', 1, true) ~= nil, true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(400, go) end
end
go()
