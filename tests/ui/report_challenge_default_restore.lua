-- After report_challenge_default (same home and config): the default it left, ticked in the
-- challenge form, is still the default after a restart, and the form opens on it, ticked.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
local proj = os.getenv("HOME") .. "/proj"

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

T.check("the default model is kept", serpis.var.get("chat-report-challenge-model"), "mock/third")
T.check("and its thinking", serpis.var.get("chat-report-challenge-thinking"), "low")

local steps = {
  function() serpis.command.run("new-chat", proj); return 600 end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    serpis.command.run("beginning-of-buffer")
    return 400
  end,
  function() click(d.rect("report-challenge")); return 800 end,
  function()
    local form = serpis.chat.review(chat).challenge_form
    T.check("the form opens on it", form and form.model, "mock/third")
    T.check("with its thinking", form and form.thinking, "low")
    T.check("ticked", form and form.remember, true)
    serpis.command.run("chat-challenge-form-cancel")
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
