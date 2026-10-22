-- A chat on another machine, end to end: the "machine" here is a real serpis-node in a
-- throwaway home of its own (its own models, pointing at serpis-mock-provider), reached the way
-- ssh reaches one, through `serpis-node agent` on stdio. The composer's machine pill moves a new
-- chat there; the chat runs there (tools read the node's files); closing the view leaves it
-- running, and it comes back whole.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd)
  local p = io.popen(cmd)
  local out = p:read("*a")
  p:close()
  return out
end
local function write(path, text)
  local f = io.open(path, "w")
  f:write(text)
  f:close()
end

-- The other machine. By default a node on this one in a throwaway home of its own; with
-- SERPIS_TEST_REMOTE_HOST (e.g. aelita@slimbook.net.ray) a real machine over ssh, where the node
-- runs in a throwaway /tmp directory and the mock provider runs next to it
-- (SERPIS_TEST_REMOTE_MOCK: its path there). serpis installs its node there from
-- SERPIS_TEST_NODE_BUILDS (a node store, as scripts/build-node.sh --store fills).
local remote = os.getenv("SERPIS_TEST_REMOTE_HOST")
local function q(s) return "'" .. s:gsub("'", "'\\''") .. "'" end
-- Run a shell command where the node runs.
local function far(cmd)
  if remote then return sh("ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new " .. remote .. " -- " .. q("sh -c " .. q(cmd))) end
  return sh(cmd)
end
-- /tmp rather than $TMPDIR: socket paths must stay short.
local box = remote and ("/tmp/snbox-" .. tostring(os.time())) or sh("mktemp -d /tmp/snbox.XXXXXX"):gsub("%s+$", "")
far("mkdir -p " .. box .. "/home/proj " .. box .. "/agent " .. box .. "/node && printf 'zero from the box\\n' > " .. box .. "/home/proj/f0.txt && printf 'one from the box\\n' > " .. box .. "/home/proj/f1.txt")
local mock_url, mock_pid = os.getenv("SERPIS_TEST_CACHE_URL"), nil
if remote then
  local out = far("nohup " .. os.getenv("SERPIS_TEST_REMOTE_MOCK") .. " " .. box .. "/mock.port " .. box .. "/mock.log >/dev/null 2>&1 & echo $!; for i in $(seq 50); do [ -s " .. box .. "/mock.port ] && break; sleep 0.1; done; cat " .. box .. "/mock.port")
  local pid, port = out:match("(%d+)%s+(%d+)")
  mock_pid, mock_url = pid, "http://127.0.0.1:" .. port
end
local models = string.format('{"providers":{"boxai":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"x","compat":{"openRouter":true},'
  .. '"models":[{"id":"anthropic/claude-sonnet-4.5","contextWindow":200000}]}}}', mock_url)
far("printf '%s' " .. q(models) .. " > " .. box .. "/agent/models.json && printf '{}' > " .. box .. "/agent/auth.json && printf '%s' "
  .. q('{"session-directory":"' .. box .. '/sessions"}') .. " > " .. box .. "/node/settings.json")
local home = remote and (box .. "/home") or sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
local env = "HOME=" .. home .. " SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock SERPIS_AGENT_DIR=" .. box .. "/agent"
serpis.var.set("remote-rayfish", false)
if remote then
  -- serpis installs its node there (remote-deploy), into the throwaway home like the rest.
  serpis.var.set("remote-ssh-args", { "-o", "StrictHostKeyChecking=accept-new" })
  serpis.var.set("remote-node-builds", os.getenv("SERPIS_TEST_NODE_BUILDS"))
  serpis.var.set("remote-hosts", { { name = "box", host = remote, env = { HOME = home, SERPIS_NODE_DIR = box .. "/node",
    SERPIS_NODE_SOCKET = box .. "/node/node.sock", SERPIS_AGENT_DIR = box .. "/agent" } } })
else
  serpis.var.set("remote-hosts", { { name = "box", command = { "sh", "-c", "exec env " .. env .. " " .. root .. "/target/debug/serpis-node agent" } } })
end

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    local wait = list[i](go)
    if wait ~= false then serpis.defer(wait or 400, go) end
  end
  go()
end
local function when(ready, f, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local function click(name)
  local r = d.composer()[name]
  if not r then return false end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
  return true
end
local function item(id)
  for _, it in ipairs(serpis.sidebar.items()) do
    if it.buffer == id then return it end
  end
end

local chat, remote_id
local function has(text) return serpis.chat.transcript(chat):find(text, 1, true) ~= nil end
steps({
  function()
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
  end,
  -- The machine pill opens the machine picker; the box is picked by typing, then a folder of
  -- its home.
  function()
    T.check("the composer shows where the chat runs", click("machine"), true)
  end,
  function(go)
    T.press("b o x RET")
    when(function() return (d.minibuffer() or ""):find("Folder on box", 1, true) ~= nil end, go)
    return false
  end,
  function(go)
    T.press("~ / p r o j RET")
    when(function() return serpis.chat.info(chat).cwd == home .. "/proj" end, go)
    return false
  end,
  function()
    local info = serpis.chat.info(chat)
    T.check("the chat now runs in the box's folder", info.cwd, home .. "/proj")
    T.check("with the box's own model", info.model, "boxai/anthropic/claude-sonnet-4.5")
    remote_id = info.session_id
    T.check("scripts see where it runs", info.machine, "box")
    T.check("the sidebar groups it under the folder on the box", item(chat) and item(chat).project, "box:" .. home .. "/proj")
  end,
  -- Typed and sent like any chat; the tools run on the box.
  function(go)
    T.press("R E A D SPC 2 RET")
    when(function() return has("done") and not serpis.chat.running(chat) end, go)
    return false
  end,
  function()
    -- (Read cards are folded: their output isn't in the transcript's text.)
    T.check("the box's files were read there", has("f0.txt") and has("f1.txt") and not has("ENOENT"), true)
    T.check("the answer came back", has("done"), true)
    T.check("its usage is shown", serpis.chat.info(chat).usage.total_tokens > 0, true)
    T.check("the box saved the session", far("ls " .. box .. "/sessions/*/ | wc -l"):match("%d+"), "1")
    if remote then
      T.check("serpis installed its node there", far("ls " .. home .. "/.serpis/bin"):find("serpis-node-", 1, true) ~= nil, true)
    end
  end,
  -- Closing the view leaves the chat running on the box; it comes back whole.
  function()
    serpis.buffer.kill(chat)
  end,
  function(go)
    serpis.command.run("remote-attach-chat", "box", remote_id)
    chat = serpis.buffer.current()
    when(function() return has("done") end, go)
    return false
  end,
  function()
    T.check("reattached, the chat shows everything", has("READ 2") and has("f1.txt") and has("done"), true)
    T.check("as the same chat", serpis.chat.info(chat).session_id, remote_id)
  end,
  function(go)
    serpis.command.run("remote-stop-node", "box")
    when(function() return far("ls " .. box .. "/node"):find("node.sock", 1, true) == nil end, go)
    return false
  end,
  function()
    T.check("the node stopped", far("ls " .. box .. "/node"):find("node.sock", 1, true), nil)
    far((mock_pid and ("kill " .. mock_pid .. "; ") or "") .. "rm -rf " .. box)
  end,
})
