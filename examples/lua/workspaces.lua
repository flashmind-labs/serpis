-- Workspaces: i3/sway-style, built into serpis (port of ~/.emacs.d/custom/workspaces.el).
--
-- Serpis keeps each workspace's live layout, so switching restores windows exactly, and
-- moving a window (or a container picked with focus-parent) carries its views along.
-- Commands: workspace-1..9, workspace-move-window-1..9, workspace (picker with preview),
-- workspace-back-and-forth, workspace-rename. Settings: workspace-auto-back-and-forth,
-- workspace-move-follows, workspace-keep-empty, workspace-initial-buffer.
-- This module only sets how workspaces are labelled; keys are bound in keybinds.lua.

local M = {}

M.NUMERALS = { "一", "二", "三", "四", "五", "六", "七", "八", "九" }

function M.setup()
  local names = {}
  for i, n in ipairs(M.NUMERALS) do
    names[tostring(i)] = n
  end
  -- Shown in the status bar ([一]) and in the workspace picker.
  serpis.var.set("workspace-display-names", names)
end

return M
