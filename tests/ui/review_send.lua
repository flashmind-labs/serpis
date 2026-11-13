-- A review waiting can be sent with nothing typed: the send button lights for it (as RET
-- sends it), and a click sends it.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
-- Scrolling jumps: a drag aimed at text must land on it, not where it scrolled to.
serpis.var.set("scroll-animation-time", 0)

local chat
local function keys(text)
  local out = {}
  for c in text:gmatch(".") do out[#out + 1] = c == " " and "SPC" or c end
  return table.concat(out, " ")
end
-- The transcript line (1-based) holding `text`, and the column it starts at.
local function find(text)
  local n = 0
  for line in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
    n = n + 1
    local at = line:find(text, 1, true)
    -- Columns count characters (the line may have "±" before it).
    if at then return n, select(2, line:sub(1, at - 1):gsub("[^\128-\191]", "")) + 1 end
  end
end
local function center(r) return r.x + r.w / 2, r.y + r.h / 2 end
local function click(r)
  local x, y = center(r)
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end
-- Select `text` in the transcript with a drag, as a person does.
local function drag_select(text)
  local line, col = find(text)
  if not line then return T.check("found " .. text, nil, "a line") end
  serpis.buffer.set_cursor(line, col)
  serpis.defer(400, function()
    local a, b = d.cell_rect(line, col), d.cell_rect(line, col + #text - 1)
    if not (a and b) then return T.check("on screen: " .. text, nil, "rects") end
    local x0, y0 = a.x + 1, a.y + a.h / 2
    local x1, y1 = b.x + b.w - 1, b.y + b.h / 2
    d.mouse_move(x0, y0)
    serpis.defer(80, function()
      d.mouse_button(x0, y0, "left", true)
      serpis.defer(80, function()
        d.mouse_move((x0 + x1) / 2, (y0 + y1) / 2)
        serpis.defer(80, function()
          d.mouse_move(x1, y1)
          serpis.defer(80, function() d.mouse_button(x1, y1, "left", false) end)
        end)
      end)
    end)
  end)
end
local function review() return serpis.chat.review(chat) end
local shots = os.getenv("SERPIS_TEST_SHOTS")
local function shot(name) if shots then d.screenshot(shots .. "/" .. name .. ".png", true) end end
local function composer() return d.composer_state().text end

local first = "This is the likely cause of the flaky retries_respect_limit test in CI."
local steps = {
  function() serpis.command.run("new-chat"); return 600 end,
  function() chat = serpis.buffer.current(); serpis.chat.prompt(chat, "REPORT:skill"); return 2500 end,
  function() serpis.command.run("chat-focus-transcript"); drag_select(first); return 1200 end,
  function() T.press("a"); return 400 end,
  function() T.press(keys("Did you reproduce it?")); return 300 end,
  function() T.press("RET"); return 600 end,
  function()
    T.check("a comment waits", review().drafts, 1)
    T.check("nothing typed", composer(), "")
    shot("1-waiting")
    local send = d.composer().send
    if send then click(send) else T.check("the send button", nil, "a rect") end
    return 2500
  end,
  function()
    T.check("a click sends it", review().drafts, 0)
    local sent = false
    for _, l in ipairs(d.chat_lines() or {}) do
      if l.text:find("Review  ·  1 question", 1, true) then sent = true end
    end
    T.check("as your message", sent, true)
    T.finish()
    return 100000
  end,
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
