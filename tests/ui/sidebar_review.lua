-- The sidebar's Review changes buttons, clicked: beside New terminal at the top, and on a
-- project's and a worktree's row when hovered.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
local out = os.getenv("SERPIS_TEST_OUT")
local tmp = sh("cd '" .. out:match("(.*)/") .. "' && pwd -P") .. "/review.d"
local repo, wt = tmp .. "/repo", tmp .. "/wt"
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. repo .. "' && cd '" .. repo .. "' && git init -q -b main && git config user.email t@t && git config user.name t"
  .. " && echo one > a.txt && git add a.txt && git commit -qm one && echo two > a.txt && git worktree add -q -b side '" .. wt .. "'")

local function click(x, y, go)
  d.mouse_move(x, y)
  serpis.defer(150, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
    serpis.defer(500, go)
  end)
end
local function row_of(kind, path)
  for _, r in ipairs(d.sidebar_rows()) do
    if r.kind == kind and r.path == path then return r end
  end
end
-- The button `n` from the right of a row (1 is the rightmost), once the pointer over the row has
-- brought them up.
local function click_row_button(row, n, size, go)
  local y = row.y + row.h / 2
  d.mouse_move(row.x + row.w / 3, y)
  serpis.defer(200, function()
    click(row.x + row.w - 8 * (size / 24) - size * (n - 0.5), y, go)
  end)
end
local function diff_root()
  if serpis.buffer.kind() ~= "diff" then return "not a diff page: " .. tostring(serpis.buffer.kind()) end
  return serpis.diff.info().root
end

local i, list = 0, nil
local function go()
  i = i + 1
  local s = list[i]
  if not s then return T.finish() end
  local wait = s(go)
  if wait ~= false then serpis.defer(wait or 400, go) end
end

local size
list = {
  function()
    serpis.var.set("sidebar-visible", true)
    serpis.project.add(repo)
    serpis.command.run("new-terminal", repo)
    return 800
  end,
  function()
    local r = d.rect("sidebar-review")
    T.check("the top row has the button", r ~= nil, true)
    size = (r.h / 32) * 24
    d.screenshot(out .. ".sidebar.png")
    click(r.x + r.w / 2, r.y + r.h / 2, go)
    return false
  end,
  function()
    T.check("the top button reviews the current folder", diff_root(), repo)
    serpis.buffer.kill(serpis.buffer.current())
    local row = row_of("project", repo)
    T.check("the project has a row", row ~= nil, true)
    click_row_button(row, 4, size, go)
    serpis.defer(100, function() d.screenshot(out .. ".hover.png") end)
    return false
  end,
  function()
    T.check("a project's button reviews that project", diff_root(), repo)
    serpis.buffer.kill(serpis.buffer.current())
    serpis.command.run("new-terminal", wt)
    return 800
  end,
  function()
    local row = row_of("worktree", wt)
    T.check("the worktree has a row", row ~= nil, true)
    if not row then return end
    -- Remove worktree, New chat here, Review changes.
    click_row_button(row, 3, size * 22 / 24, go)
    return false
  end,
  function()
    T.check("a worktree's button reviews that worktree", diff_root(), wt)
  end,
}
go()
