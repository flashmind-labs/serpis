-- Terminals in terminal nodes: this machine's (the default), and one on another machine,
-- here a serpis-node in a throwaway home of its own reached through `serpis-node agent` the
-- way ssh reaches one. They run with the home their node has, show what runs in them, open
-- where the buffer you are in works, end when their buffer is killed, and outlive the window
-- (terminal_node_restore.lua shows them again).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end

local box = sh("mktemp -d /tmp/stbox.XXXXXX"):gsub("%s+$", "")
sh("mkdir -p " .. box .. "/home/proj " .. box .. "/node && printf \"PROMPT='%%# '\\n\" > " .. box .. "/home/.zshrc")
local boxhome = sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
local env = "HOME=" .. boxhome .. " SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock SERPIS_TERMINAL_SOCKET=" .. box .. "/t.sock"
-- In the config, so the box is known when the next serpis starts (terminal_node_restore.lua).
-- (This machine's terminals in its terminal host too: they outlive the window like the box's.)
local config = string.format('serpis.var.set("remote-rayfish", false)\nserpis.var.set("terminal-persistent", true)\nserpis.var.set("remote-hosts", { { name = "box", command = { "sh", "-c", %q } } })\n',
  "exec env " .. env .. " " .. root .. "/target/debug/serpis-node agent")
sh("mkdir -p '" .. os.getenv("SERPIS_CONFIG_DIR") .. "'")
local f = io.open(os.getenv("SERPIS_CONFIG_DIR") .. "/init.lua", "w"); f:write(config); f:close()
serpis.command.run("reload-config")
f = io.open(os.getenv("SERPIS_TEST_OUT") .. ".box", "w"); f:write(box); f:close()

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
local function has(text) return (serpis.buffer.text() or ""):find(text, 1, true) ~= nil end
local function prompt() return (serpis.buffer.text() or ""):find("%%") ~= nil end
local function running() local p = serpis.buffer.process(); return p and p.running and p.name end

local here, there, pty, box_cols
local steps = {
  function(go)
    serpis.command.run("new-terminal")
    here = serpis.buffer.current()
    when(prompt, go)
    return false
  end,
  function(go)
    T.check("this machine's terminal", serpis.buffer.machine(), nil)
    -- (Typed after a moment, once the line editor reads keys.)
    serpis.buffer.insert('echo "home=$HOME;"\r')
    when(function() return has("home=" .. os.getenv("HOME") .. ";") end, go)
    return false
  end,
  function(go)
    T.check("it has the home its node has", has("home=" .. os.getenv("HOME") .. ";"), true)
    serpis.command.run("remote-new-terminal", "box", "~/proj")
    when(function() return serpis.buffer.current() ~= here and prompt() end, go)
    return false
  end,
  function(go)
    there = serpis.buffer.current()
    T.check("a terminal on the box", serpis.buffer.machine(), "box")
    serpis.buffer.insert('echo "on-box-$((6*7)) $PWD;"\r')
    when(function() return has("on-box-42 " .. boxhome .. "/proj;") end, go)
    return false
  end,
  function(go)
    T.check("it runs there, in the folder", has("on-box-42 " .. boxhome .. "/proj;"), true)
    -- Its shell knows how wide the window shows it (zsh fills the rest of a line that didn't end
    -- in a newline, and a wrong width leaves a stray % behind).
    serpis.buffer.insert('echo "cols=$(tput cols)x"\r')
    when(function() return (serpis.buffer.text() or ""):match("cols=(%d+)x") ~= nil end, go)
    return false
  end,
  function(go)
    box_cols = (serpis.buffer.text() or ""):match("cols=(%d+)x")
    -- A terminal on a PTY of the window's, in the same window: its width is the window's.
    serpis.var.set("terminal-persistent", false)
    pty = serpis.buffer.create("terminal")
    serpis.buffer.show(pty)
    serpis.var.set("terminal-persistent", true)
    when(prompt, function()
      serpis.defer(500, function()
        serpis.buffer.insert('echo "cols=$(tput cols)x"\r')
        when(function() return (serpis.buffer.text() or ""):match("cols=(%d+)x") ~= nil end, go)
      end)
    end)
    return false
  end,
  function()
    T.check("the box's shell is as wide as the window shows it", box_cols, (serpis.buffer.text() or ""):match("cols=(%d+)x"))
    serpis.buffer.kill(pty)
    serpis.buffer.show(there)
  end,
  function(go)
    serpis.buffer.insert("sleep 3\r")
    when(function() return running() == "sleep" end, go)
    return false
  end,
  function(go)
    T.check("what runs in it shows", running(), "sleep")
    when(function() return serpis.buffer.process() and not running() end, go)
    return false
  end,
  function(go)
    T.check("back at the prompt", running(), false)
    -- In a terminal on the box, a new one opens there, where it is.
    serpis.command.run("new-terminal")
    when(function() return serpis.buffer.current() ~= there and prompt() end, go)
    return false
  end,
  function(go)
    T.check("new-terminal follows the machine", serpis.buffer.machine(), "box")
    serpis.buffer.insert('echo "at=$PWD;"\r')
    when(function() return has("at=" .. boxhome .. "/proj;") end, go)
    return false
  end,
  function(go)
    T.check("in its folder", has("at=" .. boxhome .. "/proj;"), true)
    -- Killed, it ends there; the others are shown, so none is left to attach to.
    serpis.buffer.kill()
    serpis.defer(1000, function()
      serpis.command.run("terminal-attach")
      when(function() local m = serpis.debug.messages(); return m[#m] and m[#m]:find("No other terminals") end, go)
    end)
    return false
  end,
  function()
    local m = serpis.debug.messages()
    T.check("a killed terminal ends", m[#m], "No other terminals are running")
    -- Left shown at quit: terminal_node_restore.lua finds them again, still running.
    serpis.buffer.show(there)
    serpis.buffer.insert("echo left-$((6*7))-running\r")
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(800, go) end
end
go()
