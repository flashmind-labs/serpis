-- The diff page for a chat on another machine: the node there reads the repository, the window
-- compares it, and the review goes to that chat. The machine is a node in a throwaway home,
-- reached through `serpis-node agent` as ssh would.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd .. " 2>&1"); local o = p:read("*a"); p:close(); return (o:gsub("%s+$", "")) end
local function write(path, text) local f = io.open(path, "w"); f:write(text); f:close() end
local models = string.format('{"providers":{"mock":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"k","compat":{"openRouter":true},"models":[{"id":"anthropic/claude-sonnet-4.5","reasoning":false,"input":["text"],"cost":{"input":1,"output":1,"cacheRead":0.1,"cacheWrite":1},"contextWindow":200000,"maxTokens":4096}]}}}', os.getenv("SERPIS_TEST_MOCK_URL"))

local b = sh("mktemp -d /tmp/stbox.XXXXXX")
write(os.getenv("SERPIS_TEST_OUT") .. ".box", b)
sh("mkdir -p '" .. b .. "/home/proj/src' '" .. b .. "/home/.serpis/agent' '" .. b .. "/node'")
write(b .. "/home/.serpis/agent/models.json", models)
local home = sh("cd '" .. b .. "/home' && pwd -P")
local proj = home .. "/proj"
local git = "git -c user.email=t@t -c user.name=t"
sh("cd '" .. proj .. "' && git init -q -b main && printf 'fn on_the_box() {\\n    one();\\n}\\n' > src/x.rs && git add . && " .. git .. " commit -qm init")
write(proj .. "/src/x.rs", "fn on_the_box() {\n    two();\n}\n")
local env = "HOME=" .. home .. " SERPIS_AGENT_DIR=" .. home .. "/.serpis/agent SERPIS_NODE_DIR=" .. b .. "/node SERPIS_NODE_SOCKET=" .. b .. "/node/node.sock SERPIS_TERMINAL_SOCKET=" .. b .. "/t.sock"
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-offer-setup", false)
serpis.var.set("remote-hosts", { { name = "box1", command = { "sh", "-c", "exec env " .. env .. " '" .. root .. "/target/debug/serpis-node' agent" } } })

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(250, go)
end
local function info() return serpis.diff.info() end
local chat
local steps = {
  function(go)
    chat = serpis.buffer.create("chat", "*c*", { machine = "box1", cwd = proj, model = "mock/anthropic/claude-sonnet-4.5" })
    serpis.buffer.show(chat)
    when(function() return serpis.chat.info(chat) and serpis.buffer.machine(chat) == "box1" end, function()
      T.press("s-D")
      when(function() return serpis.buffer.kind() == "diff" and info().rows > 0 end, go)
    end)
    return false
  end,
  function()
    T.check("the page belongs to the machine", info().machine, "box1")
    T.check("it read the repository there", info().root, proj)
    T.check("uncommitted, on its main branch", info().compared, "Uncommitted changes")
    T.check("its file", info().file, "src/x.rs")
    local page = serpis.buffer.text()
    T.check("compared here", page:find("- 2 - |     one();", 1, true) ~= nil and page:find("+ - 2 |     two();", 1, true) ~= nil, true)
    T.press("g g")
    T.press("n")
  end,
  function(go)
    T.press("c")
    when(function() return info().writing ~= nil end, go)
    return false
  end,
  function()
    d.text("is two right")
    T.press("C-RET")
  end,
  function(go)
    T.check("a comment on it", #serpis.diff.comments(), 1)
    T.press("S")
    when(function() return (serpis.chat.transcript(chat) or ""):find("is two right", 1, true) ~= nil end, go)
    return false
  end,
  function(go)
    T.check("the review reaches the chat on the machine", (serpis.chat.transcript(chat) or ""):find("src/x.rs:2", 1, true) ~= nil, true)
    write(proj .. "/src/x.rs", "fn on_the_box() {\n    three();\n}\n")
    when(function() return serpis.buffer.current() and (function()
      for _, id in ipairs(serpis.window.buffers()) do
        if serpis.buffer.kind(id) == "diff" then return true end
      end
    end)() and serpis.buffer.text():find("three();", 1, true) ~= nil end, go)
    return false
  end,
  function()
    T.check("a change there shows up here", serpis.buffer.text():find("three();", 1, true) ~= nil, true)
    serpis.command.run("diff-select-source", "nope-not-a-branch")
  end,
  function(go)
    when(function() return info().error ~= nil end, go)
    return false
  end,
  function()
    T.check("its errors show", (info().error or ""):find("nope-not-a-branch", 1, true) ~= nil, true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(300, go) end
end
go()
