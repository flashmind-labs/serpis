-- A machine that doesn't let this account in (a rayfish peer has no account name; rayfish logs
-- in as the one the client asks for). serpis asks which account to use, carries on with what
-- it was doing, and remembers the answer. The machine is a stand-in for ssh that only lets
-- "aelita" in, and runs the node here in a throwaway home.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd)
  local p = io.popen(cmd)
  local out = p:read("*a")
  p:close()
  return out
end
local box = sh("mktemp -d /tmp/snlogin.XXXXXX"):gsub("%s+$", "")
sh("mkdir -p '" .. box .. "/home/proj' '" .. box .. "/agent' '" .. box .. "/node'")
local home = sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
local ssh = box .. "/ssh"
local f = io.open(ssh, "w")
f:write([[#!/bin/sh
host=; while [ $# -gt 0 ] && [ "$1" != "--" ]; do host="$1"; shift; done; shift
echo "$host" >> "]] .. box .. [[/logins"
case "$host" in aelita@*) exec sh -c "$*" ;; *) echo "${host%%@*}@${host#*@}: Permission denied ()." >&2; exit 255 ;; esac
]])
f:close()
sh("chmod +x '" .. ssh .. "'")
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-ssh-command", ssh)
serpis.var.set("remote-deploy", false)
serpis.var.set("remote-node-command", "env HOME=" .. home .. " SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock SERPIS_AGENT_DIR=" .. box .. "/agent " .. root .. "/target/debug/serpis-node agent")
serpis.var.set("remote-hosts", { "box.net.ray" })

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
local function prompt_has(text) return (d.minibuffer() or ""):find(text, 1, true) ~= nil end

local chat
steps({
  function()
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
    serpis.command.run("chat-select-machine")
  end,
  function(go)
    T.press("b o x . n e t . r a y RET")
    when(function() return prompt_has("Account to log in as there") end, go)
    return false
  end,
  function(go)
    T.check("a refused login asks for the account", prompt_has("box.net.ray doesn't let"), true)
    T.press("a e l i t a RET")
    -- ...and carries on: the folder picker of the machine, now reached as aelita.
    when(function() return prompt_has("Folder on box.net.ray") end, go)
    return false
  end,
  function(go)
    T.check("then goes on to pick the folder there", prompt_has("Folder on box.net.ray"), true)
    T.press("~ / p r o j RET")
    when(function() return serpis.chat.info(chat).cwd == home .. "/proj" end, go)
    return false
  end,
  function()
    T.check("the chat moved there", serpis.chat.info(chat).cwd, home .. "/proj")
    T.check("the account is remembered", serpis.var.get("remote-users")["box.net.ray"], "aelita")
    local logins = sh("cat '" .. box .. "/logins'")
    T.check("the first try was as this account, then as aelita", logins:find("^box.net.ray\n") ~= nil and logins:find("aelita@box.net.ray", 1, true) ~= nil, true)
    serpis.command.run("remote-stop-node", "box.net.ray")
  end,
  function()
    sh("rm -rf '" .. box .. "'")
  end,
})
