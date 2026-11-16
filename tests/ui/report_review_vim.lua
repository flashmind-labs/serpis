-- A report selection opened with Vim's visual-mode `c` moves to the composer in insert mode.
-- The comment is about what vim shows selected: v's last character included, V's lines whole
-- wherever the cursor stood on them.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.keys.scheme("vim")
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
serpis.var.set("scroll-animation-time", 0)

local chat
local selected = "Retries from many clients were synchronizing after an outage."
local item = "Added ±20% jitter to the backoff (src/backoff.rs:28). Retries from many clients were synchronizing after an outage. The old client did the same."
local nxt = "Removed RetryPolicy::legacy. Nothing calls it after this change."

local function find(text)
  local n = 0
  for line in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
    n = n + 1
    local at = line:find(text, 1, true)
    -- Columns count characters ("±" is two bytes).
    if at then return n, select(2, line:sub(1, at - 1):gsub("[^\128-\191]", "")) + 1 end
  end
end

local steps = {
  function() serpis.command.run("new-chat"); return 500 end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    T.press("ESC")
    local line, col = find(selected)
    T.check("report text exists", line ~= nil, true)
    if not line then return end
    serpis.buffer.set_cursor(line, col)
    T.press("v " .. string.rep("l ", #selected - 1))
    return 500
  end,
  function()
    T.check("selection is visual", table.concat(serpis.buffer.modes(), ","), "chat-selection,vim-visual")
    T.press("c")
    return 400
  end,
  function()
    local writing = serpis.chat.review(chat).writing
    T.check("c starts a change comment", writing and writing.kind, "fix")
    T.check("composer has focus", d.composer_state().focus, "prompt")
    T.check("composer is in insert mode", table.concat(serpis.buffer.modes(), ","), "chat-commenting,vim-insert")
    T.press("x")
    return 300
  end,
  function()
    local w = serpis.chat.review(chat).writing
    T.check("typing reaches the comment", w.text, "x")
    T.check("v's quote ends with the character under the cursor", w.quote, selected)
    -- ESC to normal, ESC again drops the comment.
    T.press("ESC")
    serpis.defer(200, function() T.press("ESC") end)
    return 600
  end,
  function()
    T.check("the comment is dropped", serpis.chat.review(chat).writing, nil)
    serpis.command.run("chat-focus-transcript")
    local line, col = find(selected)
    -- V from the middle of one item down into the next: both lines whole.
    serpis.buffer.set_cursor(line, col + 8)
    T.press("V j")
    return 500
  end,
  function()
    T.check("selection is visual-line", table.concat(serpis.buffer.modes(), ","), "chat-selection,vim-visual-line")
    T.press("c")
    return 400
  end,
  function()
    local w = serpis.chat.review(chat).writing
    T.check("V's quote is its lines, whole", w and w.quote, item .. "\n" .. nxt)
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
