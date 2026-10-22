-- Sessions of every machine in one list: chat-resume's "every machine" scope opens at once with
-- this machine's sessions, and the other machines' join as they answer; sessions whose chat moved
-- elsewhere aren't listed; one picked there resumes there. The machines are nodes here in
-- throwaway homes; their sessions are written for the test.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end
local function write(path, text) sh("mkdir -p '" .. path:match("(.*)/") .. "'"); local f = io.open(path, "w"); f:write(text); f:close() end
-- A session file: a header and one user message.
local function session(dir, id, cwd, text, moved_to)
  local lines = {
    string.format('{"type":"session","version":3,"id":"%s","timestamp":"2026-10-22T10:00:00.000Z","cwd":"%s"}', id, cwd),
    string.format('{"type":"message","id":"m1","parentId":null,"timestamp":"2026-10-22T10:00:01.000Z","message":{"role":"user","content":"%s","timestamp":1}}', text),
  }
  if moved_to then
    lines[#lines + 1] = string.format('{"type":"custom","id":"m2","parentId":"m1","timestamp":"2026-10-22T10:00:02.000Z","customType":"serpis-moved","data":{"to":"%s"}}', moved_to)
  end
  write(dir .. "/--x--/2026-10-22T10-00-00-000Z_" .. id .. ".jsonl", table.concat(lines, "\n") .. "\n")
end

local agent_dir = os.getenv("SERPIS_AGENT_DIR")
serpis.var.set("session-directory", agent_dir .. "/sessions")
serpis.var.set("session-directories", {})
session(agent_dir .. "/sessions", "here-1", "/tmp", "a chat here")
session(agent_dir .. "/sessions", "gone-1", "/tmp", "a chat that moved to box1", "box1")
local boxes, hosts = {}, {}
for _, name in ipairs({ "box1", "box2" }) do
  local b = sh("mktemp -d /tmp/snres.XXXXXX"):gsub("%s+$", "")
  local home = sh("mkdir -p '" .. b .. "/home/proj' && cd '" .. b .. "/home' && pwd -P"):gsub("%s+$", "")
  boxes[name] = { dir = b, home = home }
  hosts[#hosts + 1] = { name = name, command = { "sh", "-c", "sleep 0.5; exec env HOME=" .. home .. " SERPIS_AGENT_DIR=" .. home .. "/.serpis/agent SERPIS_NODE_DIR=" .. b .. "/node SERPIS_NODE_SOCKET=" .. b .. "/node/node.sock " .. root .. "/target/debug/serpis-node agent" } }
end
session(boxes.box1.home .. "/.serpis/agent/sessions", "box1-1", boxes.box1.home .. "/proj", "a chat on box1")
session(boxes.box2.home .. "/.serpis/agent/sessions", "box2-old", boxes.box2.home .. "/proj", "a chat that moved on from box2", "local")
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-offer-setup", false)
serpis.var.set("remote-hosts", hosts)
serpis.command.run("session-rescan")

local function labels()
  local out = {}
  for _, c in ipairs(d.candidates() or {}) do out[#out + 1] = c.label end
  return table.concat(out, " | ")
end
local function when(ready, f, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local steps = {
  function(go)
    -- (The rescan reads in the background.)
    serpis.defer(800, function()
      serpis.var.set("chat-resume-scope", "everywhere")
      serpis.command.run("chat-resume")
      serpis.defer(150, go)
    end)
  end,
  function(go)
    T.check("it opens at once with this machine's sessions", labels(), "a chat here  ·  here")
    T.check("while the other machines are asked", serpis.minibuffer.loading(), true)
    when(function() return not serpis.minibuffer.loading() end, go)
  end,
  function(go)
    T.check("theirs join as they answer; moved ones aren't listed", labels():gsub("a chat here  ·  here", "HERE"), "a chat on box1  ·  on box1 | HERE")
    T.press("b o x 1 RET")
    when(function() return serpis.chat.info() and serpis.chat.info().machine == "box1" end, go)
  end,
  function(go)
    T.check("one picked there resumes there", serpis.chat.info().machine, "box1")
    when(function() return serpis.chat.transcript():find("a chat on box1", 1, true) ~= nil end, go)
  end,
  function(go)
    T.check("with its conversation", serpis.chat.transcript():find("a chat on box1", 1, true) ~= nil, true)
    serpis.command.run("remote-stop-node", "box1")
    serpis.command.run("remote-stop-node", "box2")
    when(function() return sh("ls '" .. boxes.box1.dir .. "/node' '" .. boxes.box2.dir .. "/node' 2>/dev/null"):find("node.sock", 1, true) == nil end, function()
      sh("rm -rf '" .. boxes.box1.dir .. "' '" .. boxes.box2.dir .. "'")
      go()
    end)
  end,
}
local i = 0
local function go() i = i + 1; if steps[i] then steps[i](go) else T.finish() end end
go()
