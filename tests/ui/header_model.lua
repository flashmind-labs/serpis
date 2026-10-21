-- Clicking the model pill in a chat's header opens the model picker for that chat, focusing its
-- window first when another one has focus.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write('{"providers":{"fake":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"x","models":[{"id":"m"},{"id":"m2"}]}}}')
f:close()
serpis.agent.reload_models()
serpis.agent.model("fake/m")

local function click_pill(after)
  local r = d.rect("header-status-button")
  if not r then return after(false) end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
    serpis.defer(300, function() after(true) end)
  end)
end

serpis.command.run("new-chat")
local chat = serpis.buffer.current()
serpis.defer(400, function()
  click_pill(function(ok)
    T.check("the header shows a clickable model pill", ok, true)
    T.check("clicking it opens the model picker", d.minibuffer(), "Model: ")
    d.key("Escape")
    -- Another window takes focus; the pill in the chat's header still acts on the chat.
    serpis.window.split("below")
    serpis.buffer.show(serpis.buffer.scratch())
    serpis.defer(400, function()
      T.check("focus moved away", serpis.buffer.current() ~= chat, true)
      click_pill(function()
        T.check("the click focuses the chat's window", serpis.buffer.current(), chat)
        T.check("and opens its model picker", d.minibuffer(), "Model: ")
        d.key("Escape")
        T.finish()
      end)
    end)
  end)
end)
