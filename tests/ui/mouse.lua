local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
serpis.keys.scheme("vim")
local d = serpis.debug

local function shape()
  local function node(n)
    if n.buffer then return serpis.buffer.name(n.buffer) end
    local k = ({ splith = "H", splitv = "V", tabbed = "T", stacked = "S" })[n.layout]
    local parts = {}
    for _, c in ipairs(n.children) do table.insert(parts, node(c)) end
    return k .. "[" .. table.concat(parts, " ") .. "]"
  end
  return node(serpis.window.configuration().tree)
end
local function new(name)
  local id = serpis.buffer.create("text", name, { text = name })
  serpis.buffer.show(id)
end
local function win(name)
  for _, w in ipairs(serpis.window.list()) do
    if serpis.buffer.name(w.buffer) == name then return w end
  end
end
-- A point in a window's header (left part, clear of the buttons), or at fractions of its body.
local function header(name) local r = win(name).rect; return r.x + 120, r.y + 15 end
local function at(name, fx, fy) local r = win(name).rect; return r.x + r.w * fx, r.y + r.h * fy end
local function targets(kind)
  local out = {}
  for _, t in ipairs(d.drop_targets()) do if t.kind == kind then table.insert(out, t) end end
  return out
end

-- Run steps one frame-ish apart.
local queue = {}
local function step(f) table.insert(queue, f) end
local function run()
  local f = table.remove(queue, 1)
  if not f then return end
  f()
  serpis.defer(60, run)
end
-- Press at (x, y), move in a few steps to (tx, ty), release.
local function drag(x, y, tx, ty, before_release)
  step(function() d.mouse_move(x, y) end)
  step(function() d.mouse_button(x, y, "left", true) end)
  for i = 1, 4 do
    step(function() d.mouse_move(x + (tx - x) * i / 4, y + (ty - y) * i / 4) end)
  end
  if before_release then step(before_release) end
  step(function() d.mouse_button(tx, ty, "left", false) end)
  step(function() end)
end
local function click(x, y, button)
  step(function() d.mouse_move(x, y) end)
  step(function() d.mouse_button(x, y, button or "left", true) end)
  step(function() d.mouse_button(x, y, button or "left", false) end)
  step(function() end)
end

serpis.window.only(); new("a")
serpis.window.split("right"); new("b")
serpis.window.split("right"); new("c")

step(function() T.check("start", shape(), "H[a b c]") end)
-- Header onto the left edge of c: beside it.
step(function()
  local x, y = header("a"); local tx, ty = at("c", 0.08, 0.5)
  drag(x, y, tx, ty, function() T.check("dragging", d.dragging(), true) end)
  step(function() T.check("edge drop moves beside", shape(), "H[b a c]") end)
  -- Header onto the center of c: a tab group.
  step(function()
    local x, y = header("b"); local tx, ty = at("c", 0.5, 0.5)
    drag(x, y, tx, ty)
    step(function() T.check("center drop makes tabs", shape(), "H[a T[c b]]") end)
    -- Reorder: drag tab 2 before tab 1.
    step(function()
      local bar = targets("tabs")[1]
      local t1, t2 = bar.tabs[1], bar.tabs[2]
      drag(t2.x + t2.w / 2 - 20, t2.y + t2.h / 2, t1.x + 4, t1.y + t1.h / 2)
      step(function() T.check("tab reorder", shape(), "H[a T[b c]]") end)
      -- Middle-click a tab closes that view; the buffer lives on.
      step(function()
        local t = targets("tabs")[1].tabs[1]
        click(t.x + 30, t.y + t.h / 2, "middle")
        step(function()
          T.check("middle-click closes tab", shape(), "H[a c]")
          T.check("buffer survives", serpis.buffer.by_name("b") ~= nil, true)
        end)
        -- Drop on the new-workspace chip.
        step(function()
          local x, y = header("a")
          step(function() d.mouse_move(x, y) end)
          step(function() d.mouse_button(x, y, "left", true) end)
          step(function() d.mouse_move(x + 20, y + 20) end)
          step(function() d.mouse_move(x + 40, y + 40) end)
          step(function()
            local chip
            for _, t in ipairs(targets("workspace")) do
              if t.name ~= serpis.workspace.current() then chip = t end
            end
            T.check("new-workspace chip while dragging", chip ~= nil, true)
            local cx, cy = chip.rect.x + chip.rect.w / 2, chip.rect.y + chip.rect.h / 2
            table.insert(queue, 1, function() d.mouse_button(cx, cy, "left", false) end)
            table.insert(queue, 1, function() d.mouse_move(cx, cy) end)
          end)
          step(function() end)
          step(function()
            T.check("dropped onto workspace", shape(), "H[c]")
            local names = {}
            for _, w in ipairs(serpis.workspace.list()) do table.insert(names, w.name) end
            T.check("workspace 2 created", table.concat(names, ","), "1,2")
          end)
          -- ESC cancels a drag.
          step(function() serpis.window.split("right"); new("e") end)
          step(function()
            local x, y = header("e"); local tx, ty = at("c", 0.08, 0.5)
            drag(x, y, tx, ty, function() d.key("Escape") end)
            step(function() T.check("ESC cancels", shape(), "H[c e]") end)
            -- Double-click a header: fullscreen.
            step(function()
              local x, y = header("e")
              step(function() d.mouse_move(x, y) end)
              step(function() d.mouse_button(x, y, "left", true); d.mouse_button(x, y, "left", false) end)
              step(function() d.mouse_button(x, y, "left", true); d.mouse_button(x, y, "left", false) end)
              step(function() end)
              step(function()
                T.check("double-click fullscreen", serpis.window.configuration().fullscreen, true)
                serpis.command.run("window-fullscreen-toggle")
              end)
              -- Drag a whole group by its grip.
              step(function()
                serpis.window.split("right"); new("f")
                serpis.window.place(win("f").id, win("e").id, "center")
              end)
              step(function()
                T.check("group made", shape(), "H[c T[e f]]")
                local bar = targets("tabs")[1].rect
                local gx, gy = bar.x + bar.w - 10, bar.y + 11
                local tx, ty = at("c", 0.08, 0.5)
                drag(gx, gy, tx, ty)
                step(function()
                  T.check("group dragged by grip", shape(), "H[T[e f] c]")
                  T.finish()
                end)
              end)
            end)
          end)
        end)
      end)
    end)
  end)
end)
serpis.defer(500, run)
