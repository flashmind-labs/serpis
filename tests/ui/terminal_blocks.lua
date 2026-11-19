-- Block elements and box lines are drawn as shapes on whole pixels, not font glyphs (which
-- leave seams where two blocks meet). SERPIS_TEST_SHOTS: a screenshot of Claude Code's logo,
-- shades, light/heavy/double boxes and rounded corners.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
serpis.keys.scheme("vim")
serpis.command.run("new-terminal")

local function when(ready, f, tries)
  tries = tries or 150
  if ready() or tries == 0 then return f() end
  serpis.defer(100, function() when(ready, f, tries - 1) end)
end

local lines = {
  " ▐▛███▜▌   ░░▒▒▓▓██ ▀▄▌▐ ▁▂▃▄▅▆▇█",
  "▝▜█████▛▘  ╭───╮ ┌─┬─┐ ┏━┳━┓ ╔═╦═╗",
  "  ▘▘ ▝▝    │ x │ ├─┼─┤ ┣━╋━┫ ╠═╬═╣",
  "           ╰───╯ └─┴─┘ ┗━┻━┛ ╚═╩═╝ ┄┅┈┉╌╍ ╱╲╳",
}

when(function() return serpis.buffer.text():find("%S") ~= nil end, function() serpis.defer(700, function()
  -- Written by printf from octal escapes, so the command line itself stays ASCII.
  local esc = table.concat(lines, "\n"):gsub("[\128-\255]", function(b) return string.format("\\%03o", b:byte()) end)
  serpis.buffer.insert("clear; printf '" .. esc .. "\\n'\r")
  serpis.defer(1200, function()
    local text = serpis.buffer.text()
    T.check("logo printed", text:find("▝▜█████▛▘", 1, true) ~= nil, true)
    T.check("boxes printed", text:find("╚═╩═╝", 1, true) ~= nil, true)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if not shots then return T.finish() end
    serpis.debug.screenshot(shots .. "/terminal_blocks.png")
    serpis.defer(800, T.finish)
  end)
end) end)
