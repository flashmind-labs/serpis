-- Reaching a rayfish peer whose accounts aren't this one's (rayfish logs in as the account the
-- client asks for, and peers carry none): the machine list opens at once and fills in when
-- rayfish answers, a card says serpis is connecting, a refused login asks for the username, a
-- wrong one is asked again and never kept, and the right one is kept once it gets in.
-- Stand-ins: a `ray` that answers after a while, and an `ssh` that takes a second and only lets
-- "aelita" in, running the node here in a throwaway home.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd)
  local p = io.popen(cmd)
  local out = p:read("*a")
  p:close()
  return out
end
local function script(path, body)
  local f = io.open(path, "w")
  f:write(body)
  f:close()
  sh("chmod +x '" .. path .. "'")
end
local box = sh("mktemp -d /tmp/snlogin.XXXXXX"):gsub("%s+$", "")
sh("mkdir -p '" .. box .. "/home/proj' '" .. box .. "/agent' '" .. box .. "/node'")
local home = sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
script(box .. "/ray", [[#!/bin/sh
sleep 1.5
echo '{"networks":[{"name":"net","peers":[{"hostname":"box","state":"active"}]}]}'
]])
script(box .. "/ssh", [[#!/bin/sh
host=; while [ $# -gt 0 ] && [ "$1" != "--" ]; do host="$1"; shift; done; shift
echo "$host" >> "]] .. box .. [[/logins"
sleep 1
case "$host" in aelita@*) exec sh -c "$*" ;; *) echo "$host: Permission denied ()." >&2; exit 255 ;; esac
]])
serpis.var.set("remote-hosts", {})
serpis.var.set("remote-rayfish", true)
serpis.var.set("remote-rayfish-command", box .. "/ray")
serpis.var.set("remote-ssh-command", box .. "/ssh")
serpis.var.set("remote-deploy", false)
serpis.var.set("remote-node-command", "env HOME=" .. home .. " SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock SERPIS_AGENT_DIR=" .. box .. "/agent " .. root .. "/target/debug/serpis-node agent")

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
local function prompt() return d.minibuffer() or "" end
local function candidates()
  local out = {}
  for _, c in ipairs(d.candidates() or {}) do out[#out + 1] = type(c) == "table" and (c.value or c.label) or c end
  return table.concat(out, ",")
end
local function busy()
  local b = serpis.busy.list()[1]
  return b and b.label or nil
end
local function users() return (serpis.var.get("remote-users") or {})["box.net.ray"] end

local chat, saw_busy
steps({
  function()
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
    serpis.command.run("chat-select-machine")
  end,
  function(go)
    T.check("the machine list opens before rayfish answers", prompt():find("Run this chat on", 1, true) ~= nil, true)
    T.check("and shows that more is coming", serpis.minibuffer.loading(), true)
    T.check("with what is known already", candidates(), "local")
    when(function() return not serpis.minibuffer.loading() end, go)
    return false
  end,
  function()
    T.check("rayfish's peers join the open list", candidates(), "local,box.net.ray")
    T.press("b o x . n e t . r a y RET")
  end,
  -- Connecting takes a second here: a card says so meanwhile.
  function(go)
    when(function() return busy() ~= nil end, function()
      saw_busy = busy()
      when(function() return prompt() == "Username: " end, go)
    end)
    return false
  end,
  function()
    T.check("a card shows while connecting", saw_busy, "Connecting to box.net.ray…")
    T.check("it is gone once that is over", busy(), nil)
    T.check("a refused login asks for the username, briefly", prompt(), "Username: ")
    T.press("b o b RET")
  end,
  -- A wrong name is refused too: asked again, and not kept.
  function(go)
    when(function() return prompt() == "Username: " and sh("grep -c . '" .. box .. "/logins'"):match("%d+") == "2" end, go)
    return false
  end,
  function()
    T.check("a wrong name is asked again", prompt(), "Username: ")
    T.check("and is not kept", users(), nil)
    T.press("a e l i t a RET")
  end,
  function(go)
    when(function() return prompt():find("Folder on box.net.ray", 1, true) ~= nil end, go)
    return false
  end,
  function(go)
    T.check("the right one gets in, and the chat moves on", prompt():find("Folder on box.net.ray", 1, true) ~= nil, true)
    T.check("and is kept", users(), "aelita")
    T.press("~ / p r o j RET")
    when(function() return serpis.chat.info(chat).cwd == home .. "/proj" end, go)
    return false
  end,
  function()
    T.check("the chat moved there", serpis.chat.info(chat).cwd, home .. "/proj")
    T.check("tried as this account, bob, then aelita", sh("cat '" .. box .. "/logins'"), "box.net.ray\nbob@box.net.ray\naelita@box.net.ray\n")
    serpis.command.run("remote-stop-node", "box.net.ray")
  end,
  function()
    sh("rm -rf '" .. box .. "'")
  end,
})
