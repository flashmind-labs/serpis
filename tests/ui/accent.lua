-- Accent colors: themes keep their own; theme-accent picks a palette color or #rrggbb; the
-- select-accent picker previews choices live, Escape restores, Enter keeps (and remembers).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function c() return serpis.theme.colors() end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    list[i]()
    serpis.defer(200, go)
  end
  go()
end
steps({
  function()
    serpis.theme.load("modus-operandi-tinted")
    T.check("a theme keeps its own accent", c().accent, "#a60000")
    serpis.var.set("theme-accent", "purple")
    T.check("theme-accent picks a palette color", c().accent, c().purple)
    serpis.theme.load("modus-vivendi")
    T.check("and follows theme changes", c().accent, c().purple)
    serpis.var.set("theme-accent", "#ff8800")
    T.check("or any #rrggbb", c().accent, "#ff8800")
    serpis.var.set("theme-accent", "theme")
    T.check("theme restores the theme's own", c().accent, "#79a8ff")
    serpis.command.run("select-accent")
  end,
  function()
    local labels = {}
    for _, x in ipairs(d.candidates()) do labels[#labels + 1] = x.label end
    T.check("the picker offers the theme's own and its palette", table.concat(labels, ","), "theme,blue,purple,cyan,green,yellow,orange,red")
    d.key("ArrowDown")
    d.key("ArrowDown")
  end,
  function()
    T.check("moving previews the color", c().accent, c().purple)
    d.key("Escape")
  end,
  function()
    T.check("Escape restores it", c().accent, "#79a8ff")
    serpis.command.run("select-accent")
  end,
  function() d.text("cyan") end,
  function() d.key("Enter") end,
  function()
    T.check("Enter keeps the choice", serpis.var.get("theme-accent"), "cyan")
    T.check("and it applies", c().accent, c().cyan)
    local f = io.open(serpis.config_dir .. "/state.json")
    local s = f and f:read("*a") or ""
    if f then f:close() end
    T.check("and is remembered", s:find('"theme-accent": "cyan"', 1, true) ~= nil, true)
    if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/accent.png") end
  end,
})
