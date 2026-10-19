-- Workspaces: i3/sway-style window-layout slots (port of ~/.emacs.d/custom/workspaces.el).
--
-- Each workspace remembers a window configuration. Switching saves the current layout and
-- restores the target one; a never-visited workspace starts as a single *scratch* window.
-- Serpis has a single frame, so the per-frame vectors of the Emacs version become one set.
-- State lives in the `workspaces` variable, so it survives init.lua reloads.

local M = {}

M.count = 9

local NUMERALS = { "一", "二", "三", "四", "五", "六", "七", "八", "九" }

function M.numeral(i)
  return NUMERALS[i] or tostring(i)
end

-- State -------------------------------------------------------------------
-- Slots are keyed by tostring(i) so the table round-trips through the editor variable.

local function state()
  local s = serpis.var.get("workspaces")
  if type(s) ~= "table" then
    s = { current = 1, slots = {} }
  end
  s.slots = s.slots or {}
  return s
end

local function save(s)
  serpis.var.set("workspaces", s)
end

function M.current()
  return state().current
end

-- Descriptions --------------------------------------------------------------

-- Comma-separated unique project names of the buffers on screen.
local function project_label()
  local seen, names = {}, {}
  for _, id in ipairs(serpis.window.buffers()) do
    local name = serpis.project.of_buffer(id)
    if name and not seen[name] then
      seen[name] = true
      table.insert(names, name)
    end
  end
  return table.concat(names, ", ")
end

-- A workspace is "empty" when it is a single window showing *scratch*.
local function scratch_only()
  local bufs = serpis.window.buffers()
  return #bufs == 1 and bufs[1] == serpis.buffer.by_name("*scratch*")
end

-- Capture the live layout into slot i (with its label and empty flag).
local function snapshot(s, i)
  s.slots[tostring(i)] = {
    conf = serpis.window.configuration(),
    label = project_label(),
    empty = scratch_only(),
  }
end

-- Show an empty workspace: one window on *scratch*.
local function blank()
  serpis.window.only()
  serpis.buffer.show(serpis.buffer.scratch())
end

-- Switching ----------------------------------------------------------------

function M.switch(i)
  local s = state()
  if s.current == i then
    return
  end
  snapshot(s, s.current)
  local slot = s.slots[tostring(i)]
  if slot then
    serpis.window.set_configuration(slot.conf)
  else
    blank()
    snapshot(s, i)
  end
  s.current = i
  save(s)
end

-- Move the current buffer to workspace i, leaving this workspace without it.
function M.move_buffer(i)
  local s = state()
  local cur = s.current
  if cur == i then
    serpis.message(("Buffer is already on workspace %d"):format(i))
    return
  end
  local buf = serpis.buffer.current()
  local name = serpis.buffer.name(buf)

  -- 1. Remove the buffer from this workspace and remember what is left.
  if serpis.window.count() == 1 then
    serpis.buffer.show(serpis.buffer.scratch())
  else
    serpis.window.delete()
  end
  snapshot(s, cur)

  -- 2. Graft it into the target workspace.
  local slot = s.slots[tostring(i)]
  if slot then
    serpis.window.set_configuration(slot.conf)
    if scratch_only() then
      -- The target is just *scratch*: replace it instead of splitting.
      serpis.buffer.show(buf)
    else
      serpis.window.split("right")
      serpis.buffer.show(buf)
    end
  else
    serpis.window.only()
    serpis.buffer.show(buf)
  end
  snapshot(s, i)

  -- 3. Come back to the current workspace.
  serpis.window.set_configuration(s.slots[tostring(cur)].conf)
  save(s)
  serpis.message(("Moved '%s' → workspace %d"):format(name, i))
end

-- Picker with live preview (consult-workspace) ------------------------------

-- Visited, non-empty workspaces plus the current one.
local function candidates(s)
  local out = {}
  for i = 1, M.count do
    local slot = s.slots[tostring(i)]
    if slot and (i == s.current or not slot.empty) then
      local desc = (slot.label and slot.label ~= "") and slot.label or "no project"
      table.insert(out, {
        label = ("%s  %s%s"):format(M.numeral(i), desc, i == s.current and " (current)" or ""),
        value = tostring(i),
      })
    end
  end
  return out
end

function M.pick()
  local s = state()
  -- Refresh the current slot so its own preview matches the live layout.
  snapshot(s, s.current)
  save(s)
  local original = serpis.window.configuration()
  local function restore()
    serpis.window.set_configuration(original)
  end
  serpis.minibuffer.read({
    prompt = "Workspace: ",
    candidates = candidates(s),
    require_match = true,
    sort = false,
    on_select = function(value)
      local slot = value and state().slots[value]
      serpis.window.set_configuration(slot and slot.conf or original)
    end,
    on_cancel = restore,
    on_accept = function(r)
      restore()
      if r.value then
        M.switch(tonumber(r.value))
      end
    end,
  })
end

-- Setup ---------------------------------------------------------------------

function M.setup()
  local s = state()
  -- Seed the current workspace with whatever is on screen right now.
  if not s.slots[tostring(s.current)] then
    snapshot(s, s.current)
    save(s)
  end

  for i = 1, M.count do
    serpis.command.define("workspace-" .. i, function() M.switch(i) end, ("Switch to workspace %d."):format(i))
    serpis.command.define("workspace-move-buffer-" .. i, function() M.move_buffer(i) end,
      ("Move the current buffer to workspace %d."):format(i))
  end
  serpis.command.define("workspace-switch", M.pick, "Preview and switch between workspaces.")
  -- Keys are bound in keybinds.lua.

  -- Mode-line indicator: [一] in the status bar.
  serpis.ui.status("workspace", function()
    return ("[%s]"):format(M.numeral(M.current()))
  end)
end

return M
