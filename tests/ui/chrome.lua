-- Window chrome with real mouse and keys: dragging scrollbars, the sidebar toggle, the
-- growing composer.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("session-directories", {})
local function steps(list)
  local i = 0
  local function go() i = i + 1; if not list[i] then return T.finish() end; serpis.defer(list[i]() or 400, go) end
  go()
end
local function press(x, y) d.mouse_move(x, y); d.mouse_button(x, y, "left", true) end
local function release(x, y) d.mouse_button(x, y, "left", false) end
local function win()
  for _, w in ipairs(serpis.window.list()) do if w.buffer == serpis.buffer.current() then return w.rect end end
end

local lines = {}
for i = 1, 600 do lines[i] = "line " .. i end
local r, first
steps({
  function()
    local id = serpis.buffer.create("text", "*long*", { text = table.concat(lines, "\n") })
    serpis.buffer.show(id)
  end,
  function()
    T.check("starts at the top", serpis.buffer.visible(), 1)
    -- Grab the thumb at the top of the scrollbar and drag it to the middle.
    r = win()
    local x = r.x + r.w - 6
    press(x, r.y + 60)
  end,
  function() d.mouse_move(r.x + r.w - 6, r.y + 60 + (r.h - 60) / 2) end,
  function() release(r.x + r.w - 6, r.y + 60 + (r.h - 60) / 2) end,
  function()
    first = serpis.buffer.visible()
    T.check("dragging the thumb scrolls", first > 150 and first < 450, true)
    -- Clicking near the bottom of the track jumps there.
    press(r.x + r.w - 6, r.y + r.h - 4)
  end,
  function() release(r.x + r.w - 6, r.y + r.h - 4) end,
  function()
    local _, last = serpis.buffer.visible()
    T.check("clicking the track jumps", last, 600)
    -- The sidebar toggle in the status bar.
    T.check("sidebar shown", serpis.sidebar.visible(), true)
    local b = d.rect("sidebar-toggle")
    press(b.x + b.w / 2, b.y + b.h / 2)
  end,
  function()
    local b = d.rect("sidebar-toggle")
    release(b.x + b.w / 2, b.y + b.h / 2)
  end,
  function()
    T.check("toggle hides the sidebar", serpis.sidebar.visible(), false)
    local b = d.rect("sidebar-toggle")
    press(b.x + b.w / 2, b.y + b.h / 2)
  end,
  function()
    local b = d.rect("sidebar-toggle")
    release(b.x + b.w / 2, b.y + b.h / 2)
  end,
  function()
    T.check("toggle shows it again", serpis.sidebar.visible(), true)
    -- The composer grows with wrapped and multiple lines.
    serpis.command.run("new-chat")
  end,
  function()
    first = d.composer().card.h
    d.text("A long prompt that wraps: " .. string.rep("words and more words ", 20))
  end,
  function()
    local wrapped = d.composer().card.h
    T.check("composer grows when text wraps", wrapped > first + 15, true)
    T.press("S-RET"); d.text("second"); T.press("S-RET"); d.text("third")
  end,
  function()
    local h = d.composer().card.h
    T.check("composer grows with new lines", h > first + 50, true)
    T.check("composer stays under 40%", h < win().h * 0.45, true)
  end,
})
