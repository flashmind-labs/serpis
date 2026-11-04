-- Settings follow the config files on a reload: an edited default takes effect, what is no longer
-- set or defined stops applying. A value set live stays, says so, and can be reset.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local dir = os.getenv("SERPIS_CONFIG_DIR") .. "/extensions"
os.execute("mkdir -p '" .. dir .. "'")
local function ext(body)
  local f = io.open(dir .. "/probe.lua", "w")
  f:write(body)
  f:close()
  serpis.command.run("reload-config")
end
local function warned(text)
  for _, m in ipairs(serpis.debug.messages()) do
    if m:find(text, 1, true) then return true end
  end
  return false
end

-- An edited default, nothing ever set.
ext('serpis.var.define("probe-a", { x = 1, old = 1 }, "first doc")')
T.check("loaded", serpis.var.get("probe-a").x, 1)
T.check("from its default", serpis.var.info("probe-a").origin, "default")
ext('serpis.var.define("probe-a", { x = 2, new = 1 }, "second doc")')
T.check("an edited default takes effect", serpis.var.get("probe-a").x, 2)
T.check("a renamed entry is renamed", serpis.var.get("probe-a").old, nil)
T.check("the doc follows too", serpis.var.info("probe-a").doc, "second doc")

-- A definition that is gone.
ext('-- nothing')
T.check("a removed define is gone", serpis.var.get("probe-a"), nil)
T.check("nor has it any info", serpis.var.info("probe-a"), nil)

-- Set by the config, then no longer.
ext('serpis.var.define("probe-b", 1, "doc"); serpis.var.set("probe-b", 5)')
T.check("the config's value", serpis.var.get("probe-b"), 5)
T.check("from the config", serpis.var.info("probe-b").origin, "config")
ext('serpis.var.define("probe-b", 1, "doc")')
T.check("a removed set stops applying", serpis.var.get("probe-b"), 1)
ext('serpis.var.set("probe-c", 5)')
ext('-- nothing')
T.check("a set of nothing defined is gone with it", serpis.var.get("probe-c"), nil)

-- Reads during the load give what the files say.
ext('serpis.var.define("probe-d", 1, "doc"); serpis.var.set("probe-d", 5)')
ext('serpis.var.define("probe-d", 1, "doc"); serpis.var.set("probe-d-seen", serpis.var.get("probe-d"))')
T.check("the rest of the config reads the default", serpis.var.get("probe-d-seen"), 1)

-- Set before it is defined (init.lua runs first): the value stays and the doc arrives.
ext('serpis.var.set("probe-e", 5); serpis.var.define("probe-e", 1, "e doc")')
T.check("set first: value", serpis.var.get("probe-e"), 5)
T.check("set first: doc", serpis.var.info("probe-e").doc, "e doc")
T.check("set first: default", serpis.var.info("probe-e").default, 1)

-- Live values stay, and hide a new default.
ext('serpis.var.define("probe-f", 1, "doc")')
serpis.var.set("probe-f", 7)
T.check("live", serpis.var.info("probe-f").origin, "live")
ext('serpis.var.define("probe-f", 1, "doc")')
T.check("a live value survives a reload", serpis.var.get("probe-f"), 7)
ext('serpis.var.define("probe-f", 2, "doc")')
T.check("and hides a new default", serpis.var.get("probe-f"), 7)
T.check("but keeps track of it", serpis.var.info("probe-f").default, 2)
T.check("which the reload says", warned("probe-f keeps the value"), true)
local seen
serpis.hook.add("variable-changed", function(e) if e.name == "probe-f" then seen = e.value end end)
serpis.var.reset("probe-f")
T.check("reset brings the default back", serpis.var.get("probe-f"), 2)
T.check("and says so", seen, 2)
serpis.var.set("probe-f", 7)
serpis.var.set("probe-f", nil)
T.check("setting nil resets", serpis.var.get("probe-f"), 2)
ext('serpis.var.define("probe-f", 3, "doc")')
T.check("and it follows the file again", serpis.var.get("probe-f"), 3)
serpis.var.set("probe-f", 9)
serpis.command.run("reset-variable", "probe-f")
T.check("reset-variable", serpis.var.get("probe-f"), 3)

-- Built-in settings are unaffected by any of this.
T.check("built-in default", serpis.var.info("config-auto-reload").origin, "default")
T.check("built-in, nil resets", (function() serpis.var.set("font-mono", "Menlo"); serpis.var.set("font-mono", nil); return serpis.var.get("font-mono") end)(), nil)

os.remove(dir .. "/probe.lua")
serpis.command.run("reload-config")
T.finish()
