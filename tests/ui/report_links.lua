-- Code references in a chat (`src/queue.rs:142`, in a report or anywhere else) are links:
-- clicked, the diff page of the chat's folder opens on that line; a file with no changes there
-- opens itself at the line.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
-- A repository with src/queue.rs changed at line 142 (uncommitted) and src/backoff.rs as it was.
sh("mkdir -p '" .. tmp .. "/repo/src' && cd '" .. tmp .. "/repo' && git init -q -b main"
  .. " && for i in $(seq 1 200); do echo \"queue line $i\"; done > src/queue.rs"
  .. " && for i in $(seq 1 40); do echo \"backoff line $i\"; done > src/backoff.rs"
  .. " && git add . && git -c user.name=t -c user.email=t@t commit -qm init"
  .. " && sed -i '' 's/^queue line 142$/queue line 142, changed/' src/queue.rs")
local repo = sh("cd '" .. tmp .. "/repo' && pwd -P")
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

local chat
-- The transcript line (1-based) holding `text`, and the column it starts at.
local function find(text)
  local n = 0
  for line in ((serpis.chat.transcript(chat) or "") .. "\n"):gmatch("(.-)\n") do
    n = n + 1
    local at = line:find(text, 1, true)
    if at then return n, at end
  end
end
local function click_at(r)
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(120, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end
-- Put the reference on screen and click its middle.
local function click_ref(text)
  local line, col = find(text)
  if not line then return T.check("found " .. text, nil, "a line") end
  serpis.buffer.set_cursor(line, col)
  serpis.defer(500, function()
    local r = d.cell_rect(line, col + 3)
    if not r then return T.check("on screen: " .. text, nil, "a rect") end
    click_at(r)
  end)
end
local function kind() return serpis.buffer.kind(serpis.buffer.current()) end

local steps = {
  function()
    serpis.command.run("new-chat", repo)
    return 600
  end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  function()
    T.check("the report came", find("src/queue.rs:142") ~= nil, true)
    serpis.command.run("chat-focus-transcript")
    click_ref("src/queue.rs:142")
    return 2500
  end,
  function()
    T.check("the click opens the diff page", kind(), "diff")
    local i = serpis.diff.info()
    T.check("on the file", i.file, "src/queue.rs")
    T.check("on its line", i.selection and i.selection.line, 142)
    T.check("on the new side", i.selection and i.selection.side, "new")
    -- A line in a folded stretch: the fold opens.
    serpis.command.run("diff-show-line", "src/queue.rs", 20, repo)
    return 1500
  end,
  function()
    local i = serpis.diff.info()
    T.check("an unchanged line is shown too", i.selection and i.selection.line, 20)
    serpis.buffer.show(chat)
    return 600
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    click_ref("src/backoff.rs:28")
    return 2500
  end,
  function()
    T.check("a file with no changes opens itself", kind(), "text")
    T.check("at the line", (serpis.buffer.cursor()), 28)
    -- Anywhere in a chat, not only in reports: here, a plain one in your own message.
    serpis.buffer.show(chat)
    serpis.chat.prompt(chat, "Also look at src/backoff.rs:12 please")
    return 1500
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    click_ref("src/backoff.rs:12")
    return 2500
  end,
  function()
    T.check("a reference in your message is a link too", kind(), "text")
    T.check("to its line", (serpis.buffer.cursor()), 12)
    sh("rm -r '" .. tmp .. "'")
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
