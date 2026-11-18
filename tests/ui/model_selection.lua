-- Choosing a chat model must leave the default for new chats alone.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("remote-rayfish", false)
local dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. dir .. "'")
local f = assert(io.open(dir .. "/models.json", "w"))
f:write('{"providers":{"fake":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"x","models":[{"id":"m"},{"id":"m2"}]}}}')
f:close()
serpis.agent.reload_models()
serpis.var.set("agent-model", "fake/m")

local function pick(spec, key)
  serpis.command.run("chat-select-model")
  d.text(spec)
  serpis.defer(200, function() T.press(key) end)
end
local steps = {
  function() serpis.command.run("new-chat") end,
  function() pick("fake/m2", "RET") end,
  function()
    T.check("Enter selects the chat model", (serpis.chat.info() or {}).model, "fake/m2")
    T.check("Enter preserves the default", serpis.var.get("agent-model"), "fake/m")
    serpis.command.run("new-chat")
  end,
  function()
    T.check("new chats keep the original default", (serpis.chat.info() or {}).model, "fake/m")
    pick("fake/m2", "C-s")
  end,
  function()
    T.check("Ctrl+S selects the chat model", (serpis.chat.info() or {}).model, "fake/m2")
    T.check("Ctrl+S sets the default", serpis.var.get("agent-model"), "fake/m2")
    serpis.command.run("new-chat")
  end,
  function()
    T.check("new chats use the explicit default", (serpis.chat.info() or {}).model, "fake/m2")
    serpis.command.run("chat-select-model", "fake/m")
  end,
  function()
    T.check("command parameter selects the model", (serpis.chat.info() or {}).model, "fake/m")
    T.check("command parameter preserves the default", serpis.var.get("agent-model"), "fake/m2")
    -- The alternate action also works in the bottom minibuffer.
    serpis.var.set("minibuffer-style", "bottom")
    pick("fake/m", "C-s")
  end,
  function()
    T.check("Ctrl+S can make the current model the default", serpis.var.get("agent-model"), "fake/m")
    pick("no-such-model", "C-s")
  end,
  function()
    T.check("Ctrl+S with no match keeps the picker open", d.minibuffer(), "Model: ")
    T.check("no match preserves the default", serpis.var.get("agent-model"), "fake/m")
    T.press("ESC")
    pick("fake/m2", "ESC")
  end,
  function()
    T.check("cancel preserves the chat model", (serpis.chat.info() or {}).model, "fake/m")
    T.check("cancel preserves the default", serpis.var.get("agent-model"), "fake/m")
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  steps[i]()
  serpis.defer(700, go)
end
go()
