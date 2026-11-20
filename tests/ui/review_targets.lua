-- Sending a review asks which chat it goes to: the chats used most recently come first.
-- Three chats, used in turn, then the first again; a diff page opened from a text buffer (so
-- it has no chat of its own) lists that one first. Everything in a throwaway repo.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local function sh(cmd) local p = io.popen(cmd .. " 2>&1"); local o = p:read("*a"); p:close(); return (o:gsub("%s+$", "")) end
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})

local repo = sh("cd \"$(mktemp -d /tmp/srev.XXXXXX)\" && pwd -P")
sh("cd '" .. repo .. "' && printf 'one\\n' > a.txt && git init -q -b main && git add . && git -c user.email=t@t -c user.name=t commit -qm init && printf 'two\\n' > a.txt")

local chats = {}
local function chat(name)
  local id = serpis.buffer.create("chat", "*" .. name .. "*", { cwd = repo })
  serpis.buffer.show(id)
  serpis.chat.prompt(id, name)
  chats[name] = id
end
local function labels()
  local out = {}
  for _, c in ipairs(d.candidates()) do out[#out + 1] = c.label end
  return table.concat(out, ",")
end

local steps = {
  function() chat("alpha") end,
  function() chat("bravo") end,
  function() chat("charlie") end,
  -- Back to the first: it's now the most recent.
  function() serpis.buffer.show(chats.alpha) end,
  function()
    local text = serpis.buffer.create("text", "*notes*", { text = "" })
    serpis.buffer.show(text)
  end,
  function() serpis.diff.open({ dir = repo }) end,
  function()
    serpis.diff.comment({ text = "look", line = 1 })
    serpis.command.run("diff-send-review")
  end,
  function()
    T.check("the picker asks", d.minibuffer(), "Send the review to: ")
    -- Then chats never focused (the one serpis starts with), then a new one.
    T.check("most recently used first", labels():match("^alpha,charlie,bravo,") ~= nil, true)
    T.check("a new chat last", labels():match(",New chat here$") ~= nil, true)
    T.press("ESC")
    T.finish()
  end,
}
local function run(i)
  if i > #steps then return end
  steps[i]()
  serpis.defer(1200, function() run(i + 1) end)
end
serpis.defer(600, function() run(1) end)
