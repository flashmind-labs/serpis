-- An agent's prose wraps at chat-measure characters; code, tables and cards keep the column.
-- A smoke test (the window manager may resize the window, so widths aren't asserted here; the
-- wrap itself is in text.rs's tests). SERPIS_TEST_SHOTS=dir saves both looks to compare.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
local shots = os.getenv("SERPIS_TEST_SHOTS")
local steps = {
  function() serpis.command.run("new-chat"); return 500 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "LONG"); return 3000 end,
  function()
    serpis.command.run("chat-focus-transcript")
    serpis.command.run("beginning-of-buffer")
    return 800
  end,
  function()
    if shots then d.screenshot(shots .. "/measure-on.png") end
    return 800
  end,
  function()
    serpis.var.set("chat-measure", 0)
    return 800
  end,
  function()
    if shots then d.screenshot(shots .. "/measure-off.png") end
    T.check("the reply came", (serpis.chat.transcript() or ""):find("Section 40", 1, true) ~= nil, true)
    return 800
  end,
  function() T.finish(); return 100000 end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return end
  local ok, wait = pcall(steps[i])
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
