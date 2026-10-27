-- Pairing a phone, the desktop's side: phone-pair shows a code whose link holds the machine (as
-- ssh reaches it, with the host key trusted here) and a pairing key the machine now lets in only
-- to run `serpis-node pair`. The phone's offer (here: that command, run as the pairing key's
-- login would) reaches the desktop, which asks; once you say yes, the phone's key is let in, the
-- pairing key isn't, and the code says the phone is paired.
-- This machine is shared the same way, with what ssh can't say: an address you pick (serpis
-- offers the ones it finds), its host keys read from sshd's files, the port its config gives.
-- Stand-ins: an `ssh` for one machine "box" (its node runs here, in a throwaway home), a
-- `tailscale` with one name for this machine, host key and sshd config files for it.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local node = root .. "/target/debug/serpis-node"
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

local box = sh("mktemp -d /tmp/sppair.XXXXXX"):gsub("%s+$", "")
sh("mkdir -p '" .. box .. "/home/.ssh' '" .. box .. "/agent' '" .. box .. "/node'")
local nodehome = sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
local nodeenv = "HOME=" .. nodehome .. " SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock SERPIS_AGENT_DIR=" .. box .. "/agent SERPIS_TERMINAL_SOCKET=" .. box .. "/t.sock"
sh("ssh-keygen -q -t ed25519 -N '' -f '" .. box .. "/hk'")
local hostkey = sh("cut -d' ' -f1,2 '" .. box .. "/hk.pub'"):gsub("%s+$", "")
-- The box's host key is trusted here already (it was added before).
write(box .. "/known_hosts", "box " .. hostkey .. "\n")
-- The phone's key.
sh("ssh-keygen -q -t ed25519 -N '' -f '" .. box .. "/phone'")
local phonekey = sh("cut -d' ' -f1,2 '" .. box .. "/phone.pub'"):gsub("%s+$", "")

write(box .. "/ssh", [[#!/bin/sh
host=; g=
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
  case "$1" in
    -o|-p|-i|-F) shift ;;
    -G) g=1 ;;
    -*) ;;
    *) host="$1" ;;
  esac
  shift
done
if [ -n "$g" ]; then printf 'user me\nhostname %s\nport 22\n' "$host"; exit 0; fi
shift
exec sh -c "$*"
]], true)

serpis.var.set("remote-hosts", {})
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-ssh-command", box .. "/ssh")
serpis.var.set("remote-known-hosts", box .. "/known_hosts")
serpis.var.set("remote-ssh-known-hosts", false)
serpis.var.set("remote-deploy", false)
serpis.var.set("remote-node-command", "env " .. nodeenv .. " " .. node .. " agent")
serpis.var.set("remote-machines", { { name = "box", host = "box" } })

-- This machine, as the test lets serpis see it: one host key, an sshd config with a port of its
-- own, and a tailscale that gives it one name.
sh("ssh-keygen -q -t ed25519 -N '' -f '" .. box .. "/lhk'")
local localkey = sh("cut -d' ' -f1,2 '" .. box .. "/lhk.pub'"):gsub("%s+$", "")
write(box .. "/sshd_config", "#Port 22\nPort 2022\n")
write(box .. "/tailscale", "#!/bin/sh\nprintf '%s' '{\"Self\":{\"DNSName\":\"mac.tail1234.ts.net.\",\"TailscaleIPs\":[\"100.64.0.1\"]}}'\n", true)
serpis.var.set("remote-tailscale-command", box .. "/tailscale")
serpis.var.set("phone-pair-local-host-keys", box .. "/lhk*.pub")
serpis.var.set("phone-pair-sshd-config", box .. "/sshd_config")
serpis.var.set("phone-pair-local-address", "")
local paired
serpis.hook.add("phone-paired", function(p) paired = p.label .. " on " .. table.concat(p.machines, ",") end)

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then
      for _, w in ipairs({ "stop", "stop-terminals" }) do os.execute("env " .. nodeenv .. " " .. node .. " " .. w .. " >/dev/null 2>&1") end
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
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local function text() return serpis.buffer.text() or "" end
local function keys() return read(nodehome .. "/.ssh/authorized_keys") or "" end
-- This machine's own: the node the window's chats run in is reached in this test's home.
local function here() return read(os.getenv("HOME") .. "/.ssh/authorized_keys") or "" end
local function candidates()
  local out = {}
  for _, c in ipairs(d.candidates() or {}) do out[#out + 1] = type(c) == "table" and (c.value or c.label) or c end
  return "," .. table.concat(out, ",") .. ","
end
local payload = {}

steps({
  function(go)
    serpis.command.run("phone-pair", "box")
    when(function() return text():find("serpis://pair#", 1, true) end, go)
    return false
  end,
  function()
    T.check("a code to scan, for the box", text():find("Shares box", 1, true) ~= nil, true)
    -- What the link holds (decoded as the phone does).
    local url = text():match("serpis://pair#([%w_%-]+)")
    -- (With SERPIS_TEST_SHOTS: the code as drawn, and its link, to read the one against the other.)
    if os.getenv("SERPIS_TEST_SHOTS") then
      d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/phone_pair.png")
      write(os.getenv("SERPIS_TEST_SHOTS") .. "/phone_pair.url", "serpis://pair#" .. url)
    end
    local fields = sh("printf %s '" .. url .. "' | python3 -c \"import base64,json,sys; d=sys.stdin.read(); v=json.loads(base64.urlsafe_b64decode(d+'='*(-len(d)%4))); m=v['m'][0]; print(v['p']); print(m['n'], m['h'], m['p'], m['u']); print(m['k'][0]); print(len(v['k']) > 100)\"")
    local lines = {}
    for l in fields:gmatch("[^\n]+") do lines[#lines + 1] = l end
    payload.id = lines[1]
    T.check("the machine as ssh reaches it", lines[2], "box box 22 me")
    T.check("with the host key trusted here", lines[3], hostkey)
    T.check("and a pairing key", lines[4], "True")
    local line = keys():match("[^\n]*serpis%-pairing " .. payload.id .. "[^\n]*") or ""
    T.check("the box lets the pairing key in, only to pair", line:find("restrict,command=\"'", 1, true) == 1 and line:find("' pair " .. payload.id .. "\",expiry-time=\"", 1, true) ~= nil, true)
    -- The phone logs in with it and offers its key.
    os.execute("printf '%s\\n' '{\"key\":\"" .. phonekey .. "\",\"label\":\"iPhone\"}' | env " .. nodeenv .. " " .. node .. " pair " .. payload.id .. " > '" .. box .. "/answer' 2>&1 &")
  end,
  function(go) when(function() return (d.minibuffer() or ""):find("Let iPhone", 1, true) end, go); return false end,
  function()
    local print = sh("ssh-keygen -lf '" .. box .. "/phone.pub' | cut -d' ' -f2"):gsub("%s+$", "")
    T.check("you're asked, with its fingerprint", d.minibuffer(), "Let iPhone (" .. print .. ") into box? (y or n) ")
    T.press("y")
  end,
  function(go) when(function() return (read(box .. "/answer") or ""):find("approved") and text():find("Paired", 1, true) end, go); return false end,
  function()
    T.check("the phone hears yes", (read(box .. "/answer") or ""):gsub("%s+$", ""), '{"approved":true}')
    T.check("its key is let in", keys():find(phonekey .. " serpis phone iPhone", 1, true) ~= nil, true)
    T.check("the pairing key isn't any more", keys():find("serpis-pairing", 1, true), nil)
    T.check("the code says so", text(), "Paired: iPhone can reach box")
    T.check("phone-paired says so", paired, "iPhone on box")
  end,
  -- Turned down, and given up (the code's buffer closed): the pairing key goes either way.
  function(go)
    serpis.command.run("phone-pair", "box")
    when(function() return text():find("serpis://pair#", 1, true) end, go)
    return false
  end,
  function()
    local url = text():match("serpis://pair#([%w_%-]+)")
    payload.id = sh("printf %s '" .. url .. "' | python3 -c \"import base64,json,sys; d=sys.stdin.read(); print(json.loads(base64.urlsafe_b64decode(d+'='*(-len(d)%4)))['p'])\""):gsub("%s+$", "")
    os.execute("printf '%s\\n' '{\"key\":\"" .. phonekey .. "\",\"label\":\"Someone\"}' | env " .. nodeenv .. " " .. node .. " pair " .. payload.id .. " > '" .. box .. "/answer2' 2>&1 &")
  end,
  function(go) when(function() return (d.minibuffer() or ""):find("Let Someone", 1, true) end, go); return false end,
  function() T.press("n") end,
  function(go) when(function() return (read(box .. "/answer2") or ""):find("approved") end, go); return false end,
  function()
    T.check("turned down, the phone hears no", (read(box .. "/answer2") or ""):gsub("%s+$", ""), '{"approved":false}')
    T.check("and the code says so", text(), "Turned down: Someone wasn't let in")
    T.check("its pairing key is gone", keys():find("serpis-pairing", 1, true), nil)
    serpis.command.run("phone-pair", "box")
  end,
  function(go) when(function() return keys():find("serpis-pairing", 1, true) end, go); return false end,
  function() serpis.buffer.kill(serpis.buffer.current()) end,
  function(go) when(function() return not keys():find("serpis-pairing", 1, true) end, go); return false end,
  function() T.check("a code closed unused lets nothing in", keys():find("serpis-pairing", 1, true), nil) end,
  -- This machine: the phone reaches the node this window's own chats run in, so it needs an
  -- address, which ssh can't give for this machine.
  function(go)
    serpis.command.run("phone-pair", "local")
    when(function() return (d.minibuffer() or "") == "The phone reaches this machine at: " and not serpis.minibuffer.loading() end, go)
    return false
  end,
  function()
    T.check("sharing this machine asks for an address the phone can use", d.minibuffer(), "The phone reaches this machine at: ")
    T.check("its Tailscale name is offered", candidates():find(",mac.tail1234.ts.net,", 1, true) ~= nil, true)
    T.press("t a i l 1 2 3 4 RET")
  end,
  function(go) when(function() return text():find("serpis://pair#", 1, true) end, go); return false end,
  function()
    T.check("the address picked is kept", serpis.var.get("phone-pair-local-address"), "mac.tail1234.ts.net")
    T.check("the code says what it shares", text():find("Shares this machine (mac.tail1234.ts.net)", 1, true) ~= nil, true)
    T.check("and that nothing answers ssh here", text():find("Nothing answers ssh on port 2022 here", 1, true) ~= nil, true)
    local url = text():match("serpis://pair#([%w_%-]+)")
    local fields = sh("printf %s '" .. url .. "' | python3 -c \"import base64,json,sys; d=sys.stdin.read(); v=json.loads(base64.urlsafe_b64decode(d+'='*(-len(d)%4))); m=v['m'][0]; print(v['p']); print(len(v['m'])); print(m['n'], m['h'], m['p'], m['u']); print(m['k'][0])\"")
    local lines = {}
    for l in fields:gmatch("[^\n]+") do lines[#lines + 1] = l end
    payload.id = lines[1]
    local name = sh("hostname -s 2>/dev/null || hostname"):gsub("%s+$", ""):gsub("%.local$", "")
    T.check("this machine alone is shared", lines[2], "1")
    T.check("as the phone reaches it: the address, sshd's port, this account", lines[3], name .. " mac.tail1234.ts.net 2022 " .. os.getenv("USER"))
    T.check("with its own host key, read from sshd's files", lines[4], localkey)
    local line = here():match("[^\n]*serpis%-pairing " .. payload.id .. "[^\n]*") or ""
    T.check("this machine lets the pairing key in, only to pair", line:find("restrict,command=\"'", 1, true) == 1, true)
    -- The phone logs in with it: the node it reaches is this window's own.
    os.execute("printf '%s\\n' '{\"key\":\"" .. phonekey .. "\",\"label\":\"iPhone\"}' | " .. node .. " pair " .. payload.id .. " > '" .. box .. "/answer3' 2>&1 &")
  end,
  function(go) when(function() return (d.minibuffer() or ""):find("Let iPhone", 1, true) end, go); return false end,
  function()
    local print = sh("ssh-keygen -lf '" .. box .. "/phone.pub' | cut -d' ' -f2"):gsub("%s+$", "")
    T.check("you're asked for this machine too", d.minibuffer(), "Let iPhone (" .. print .. ") into this machine? (y or n) ")
    T.press("y")
  end,
  function(go) when(function() return (read(box .. "/answer3") or ""):find("approved") and text():find("Paired", 1, true) end, go); return false end,
  function()
    T.check("the phone hears yes", (read(box .. "/answer3") or ""):gsub("%s+$", ""), '{"approved":true}')
    T.check("its key is let in here", here():find(phonekey .. " serpis phone iPhone", 1, true) ~= nil, true)
    T.check("the pairing key isn't any more", here():find("serpis-pairing", 1, true), nil)
    T.check("the code says so", text(), "Paired: iPhone can reach this machine")
    T.check("phone-paired says so", paired, "iPhone on local")
  end,
})
