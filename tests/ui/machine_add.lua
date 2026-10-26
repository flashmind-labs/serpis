-- Adding a machine: machine-add suggests what your ssh config and Tailscale know (not phones),
-- then checks the machine typed in. Its host key is unknown: its fingerprint is shown and, once
-- trusted, kept in serpis's own known_hosts (ssh's is never written). It doesn't take your key:
-- ssh-copy-id runs in a terminal, and when it ends the machine is checked again, named, kept
-- (remote-machines) and connected. Renamed, its host key forgotten, removed. The machine picker
-- offers "Add a machine…".
-- Stand-ins: an `ssh` that knows one host key and lets you in once ssh-copy-id has run (then
-- runs the node here, in a throwaway home), an `ssh-copy-id`, a `tailscale`.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd)
  local p = io.popen(cmd)
  local out = p:read("*a")
  p:close()
  return out
end
local function write(path, body, exec)
  local f = io.open(path, "w")
  f:write(body)
  f:close()
  if exec then sh("chmod +x '" .. path .. "'") end
end
local function read(path)
  local f = io.open(path)
  if not f then return nil end
  local s = f:read("*a")
  f:close()
  return s
end

local box = sh("mktemp -d /tmp/smadd.XXXXXX"):gsub("%s+$", "")
sh("mkdir -p '" .. box .. "/home' '" .. box .. "/agent' '" .. box .. "/node'")
local nodehome = sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
-- The box's host key, as a known_hosts line.
sh("ssh-keygen -q -t ed25519 -N '' -f '" .. box .. "/hk'")
local keyline = "box " .. sh("cut -d' ' -f1,2 '" .. box .. "/hk.pub'"):gsub("%s+$", "")
local print = sh("ssh-keygen -lf '" .. box .. "/hk.pub' | cut -d' ' -f2"):gsub("%s+$", "")

write(box .. "/ssh", [[#!/bin/sh
khf=; strict=; host=
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
  case "$1" in
    -o) shift
        case "$1" in
          UserKnownHostsFile=*) [ -z "$khf" ] && khf="${1#UserKnownHostsFile=}" ;;
          StrictHostKeyChecking=*) [ -z "$strict" ] && strict="${1#StrictHostKeyChecking=}" ;;
        esac ;;
    -*) ;;
    *) host="$1" ;;
  esac
  shift
done
shift
cmd="$*"
echo "$host strict=$strict files=$khf" >> "]] .. box .. [[/logins"
key="]] .. keyline .. [["
eval "set -- $khf"
known=
for f; do grep -qxF "$key" "$f" 2>/dev/null && known=1; done
if [ -z "$known" ]; then
  if [ "$strict" = accept-new ]; then echo "$key" >> "$1"; else
    echo "No ED25519 host key is known for box and you have requested strict checking." >&2
    echo "Host key verification failed." >&2
    exit 255
  fi
fi
[ -f "]] .. box .. [[/authorized" ] || { echo "$host: Permission denied (publickey,password)." >&2; exit 255; }
exec sh -c "$cmd"
]], true)
write(box .. "/ssh-copy-id", [[#!/bin/sh
echo "$@" > "]] .. box .. [[/copied"
touch "]] .. box .. [[/authorized"
echo "Number of key(s) added: 1"
]], true)
write(box .. "/tailscale", [[#!/bin/sh
echo '{"Peer":{"a":{"DNSName":"box.tail1.ts.net.","OS":"linux","Online":true},"b":{"DNSName":"phone.tail1.ts.net.","OS":"iOS","Online":true}}}'
]], true)
-- This (throwaway) home's ssh: a config naming a host, and a key to copy.
local home = os.getenv("HOME")
sh("mkdir -p '" .. home .. "/.ssh'")
write(home .. "/.ssh/config", "Host gpu-box\n  HostName 10.0.0.5\nHost *\n")
sh("cp '" .. box .. "/hk.pub' '" .. home .. "/.ssh/id_ed25519.pub'")

serpis.var.set("remote-hosts", {})
serpis.var.set("remote-rayfish", false)
serpis.var.set("machine-suggest-lan", false)
serpis.var.set("remote-ssh-command", box .. "/ssh")
serpis.var.set("remote-ssh-copy-id-command", box .. "/ssh-copy-id")
serpis.var.set("remote-tailscale-command", box .. "/tailscale")
serpis.var.set("remote-known-hosts", box .. "/known_hosts")
serpis.var.set("remote-deploy", false)
serpis.var.set("remote-node-command", "env HOME=" .. nodehome .. " SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock SERPIS_AGENT_DIR=" .. box .. "/agent " .. root .. "/target/debug/serpis-node agent")
local connected
serpis.hook.add("remote-connected", function(p) if p.machine == "box" then connected = true end end)
local added_hook
serpis.hook.add("machine-added", function(p) added_hook = p.name .. "=" .. p.host end)

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then
      for _, w in ipairs({ "stop", "stop-terminals" }) do
        os.execute("env HOME=" .. nodehome .. " SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock " .. root .. "/target/debug/serpis-node " .. w .. " >/dev/null 2>&1")
      end
      os.execute("command rm -rf '" .. box .. "'")
      return T.finish()
    end
    local wait = list[i](go)
    if wait ~= false then serpis.defer(wait or 300, go) end
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
  return "," .. table.concat(out, ",") .. ","
end
local function machines()
  local out = {}
  for _, m in ipairs(serpis.var.get("remote-machines") or {}) do out[#out + 1] = m.name .. "=" .. m.host end
  return table.concat(out, ",")
end

steps({
  function(go)
    serpis.command.run("machine-add")
    when(function() return not serpis.minibuffer.loading() end, go)
    return false
  end,
  function()
    T.check("it asks where", prompt(), "Add a machine: ")
    T.check("the ssh config's hosts are suggested", candidates():find(",gpu-box,", 1, true) ~= nil, true)
    T.check("not its patterns", candidates():find(",*,", 1, true), nil)
    T.check("Tailscale's machines are", candidates():find(",box.tail1.ts.net,", 1, true) ~= nil, true)
    T.check("not its phones", candidates():find("phone", 1, true), nil)
    T.press("C-g")
  end,
  function(go)
    serpis.command.run("machine-add", "box")
    when(function() return prompt():find("Trust it?", 1, true) end, go)
    return false
  end,
  function()
    T.check("an unknown host key is shown before it is trusted", prompt(), "box's host key is " .. print .. " (ED25519). Trust it? (y or n) ")
    T.press("y")
  end,
  function(go) when(function() return prompt():find("doesn't take your key", 1, true) end, go); return false end,
  function()
    T.check("trusted, it is kept in serpis's known_hosts", (read(box .. "/known_hosts") or ""):find(keyline, 1, true) ~= nil, true)
    T.check("a refused key: what to do about it", candidates(), ",Copy my key there,Log in as another account,Copy my public key to the clipboard,")
    T.press("RET")
  end,
  function(go) when(function() return prompt() == "Call it: " end, go); return false end,
  function()
    local copied = read(box .. "/copied") or ""
    T.check("ssh-copy-id ran, for the box", copied:find("'?box'?%s*$") ~= nil, true)
    T.check("trusting what serpis trusts", copied:find("UserKnownHostsFile=" .. box .. "/known_hosts", 1, true) ~= nil, true)
    T.check("then it is checked again, and named (its host, by default)", d.minibuffer_input(), "box")
    T.press("RET")
  end,
  function(go) when(function() return connected end, go); return false end,
  function()
    T.check("it is kept", machines(), "box=box")
    T.check("machine-added says so", added_hook, "box=box")
    T.check("and connected (its node runs)", connected, true)
    T.check("ssh's own known_hosts is never written", read(home .. "/.ssh/known_hosts"), nil)
    T.check("every login checks host keys strictly", (read(box .. "/logins") or ""):find("strict=yes", 1, true) ~= nil, true)
    serpis.command.run("remote-new-chat")
  end,
  function()
    T.check("the machine picker offers to add one", candidates():find(",Add a machine…,", 1, true) ~= nil, true)
    T.press("C-g")
  end,
  function()
    serpis.command.run("machine-rename", "box", "lab")
    T.check("renamed", machines(), "lab=box")
    serpis.command.run("machine-forget-host-key", "lab")
    T.check("its host key forgotten", (read(box .. "/known_hosts") or ""):find(keyline, 1, true), nil)
    serpis.command.run("machine-remove", "lab")
    T.check("removed", machines(), "")
    serpis.command.run("remote-new-chat")
  end,
  function()
    T.check("with no machines, the picker is machine-add", prompt(), "Add a machine: ")
    T.press("C-g")
  end,
})
