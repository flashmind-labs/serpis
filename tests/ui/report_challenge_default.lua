-- "default from now on" says whether the challenge's model and thinking are the default, and
-- makes it so: unticked, a challenge on another model leaves none, and the next one is set up
-- on the chat's own model; ticked, they are the next one's, which opens with it ticked;
-- unticked then, the default goes.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"},{"id":"other"},{"id":"third"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)

local proj = os.getenv("HOME") .. "/proj"
os.execute("mkdir -p '" .. proj .. "' && cd '" .. proj .. "' && git init -q && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m start")

local chat
local function click(r)
  if not r then return end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end
local function form() return serpis.chat.review(chat).challenge_form end
-- Open the form from the report's header (Challenge, or Again once one ran).
local function open()
  serpis.command.run("chat-focus-transcript")
  serpis.command.run("beginning-of-buffer")
  serpis.defer(400, function() click(d.rect("report-challenge") or d.rect("report-challenge-again")) end)
end
local function default() return serpis.var.get("chat-report-challenge-model") end

local steps = {
  function() serpis.command.run("new-chat", proj); return 600 end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  function() open(); return 1000 end,
  function()
    T.check("first: on the chat's model", form() and form().model, "mock/m")
    T.check("first: no default, unticked", form() and form().remember, false)
    -- Another model and thinking, left unticked.
    serpis.command.run("chat-challenge-form-model", "mock/other")
    click(d.rect("challenge-thinking"))
    return 500
  end,
  function()
    T.press("RET")
    return 300
  end,
  function()
    T.check("it started", form(), nil)
    T.check("unticked: no default model", default(), nil)
    T.check("unticked: no default thinking", serpis.var.get("chat-report-challenge-thinking"), nil)
    return 5000
  end,
  function()
    local run = serpis.chat.challenges(chat)[1]
    T.check("on the model chosen", run and run.model, "mock/other")
    T.check("it has ended", run and run.running, false)
    open()
    return 1000
  end,
  function()
    T.check("again: on the chat's model, not the last challenge's", form() and form().model, "mock/m")
    T.check("again: the default thinking", form() and form().thinking, nil)
    -- Ticked this time: they stay.
    serpis.command.run("chat-challenge-form-model", "mock/third")
    click(d.rect("challenge-remember"))
    return 500
  end,
  function()
    T.check("ticked", form() and form().remember, true)
    T.press("RET")
    return 5000
  end,
  function()
    T.check("ticked: the default now", default(), "mock/third")
    open()
    return 1000
  end,
  function()
    T.check("then: on the default", form() and form().model, "mock/third")
    T.check("then: ticked, as it is the default", form() and form().remember, true)
    -- Another model, the box unticked: the default goes.
    serpis.command.run("chat-challenge-form-model", "mock/other")
    click(d.rect("challenge-remember"))
    return 500
  end,
  function()
    T.check("unticked", form() and form().remember, false)
    T.press("RET")
    return 5000
  end,
  function()
    T.check("unticked: the default is gone", default(), nil)
    open()
    return 1000
  end,
  function()
    T.check("last: on the chat's model again", form() and form().model, "mock/m")
    T.check("last: unticked", form() and form().remember, false)
    T.finish()
  end,
}

local i = 0
local function go()
  i = i + 1
  local step = steps[i]
  if not step then return end
  local ok, wait = pcall(step)
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
