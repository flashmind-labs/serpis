-- Config files reload on save only with config-auto-reload on (off by default); otherwise
-- reload-config does it.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local dir = os.getenv("SERPIS_CONFIG_DIR") .. "/extensions"
os.execute("mkdir -p '" .. dir .. "'")
local function write(cmd)
  local f = io.open(dir .. "/probe.lua", "w")
  f:write('serpis.command.define("' .. cmd .. '", function() end)\n')
  f:close()
end
local function has(cmd)
  for _, c in ipairs(serpis.command.list()) do if c == cmd then return true end end
  return false
end
local steps = {
  function() T.check("off by default", serpis.var.get("config-auto-reload"), false); write("probe-one") end,
  function() T.check("off: a saved file isn't loaded by itself", has("probe-one"), false); serpis.command.run("reload-config") end,
  function() T.check("reload-config loads it", has("probe-one"), true); serpis.var.set("config-auto-reload", true) end,
  function() write("probe-two") end,
  function() T.check("on: saving reloads", has("probe-two"), true); serpis.var.set("config-auto-reload", false) end,
  function() write("probe-three") end,
  function() T.check("off again: nothing reloads", has("probe-three"), false); os.remove(dir .. "/probe.lua") end,
}
local i = 0
local function go() i = i + 1; if steps[i] then steps[i](); serpis.defer(900, go) else T.finish() end end
go()
