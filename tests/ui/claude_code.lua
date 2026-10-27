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
  function(go) return ask("please edit", go) end,
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
  function()
    T.check("chat-answer-questions takes answers from a script", has("You chose Red in L."), true)
  end,
  function()
    local log = sh("cat '" .. os.getenv("FAKE_CLAUDE_LOG") .. "'")
    T.check("Always passed Claude Code's suggestion back", log:find('"updatedPermissions":%[{"destination":"session","mode":"acceptEdits"') ~= nil, true)
    T.check("the chat's session is Claude Code's", log:find('"--session-id"', 1, true) ~= nil, true)
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
