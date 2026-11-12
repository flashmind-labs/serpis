-- Not part of the suite: renders an agent's report (report.md, through the mock model) in
-- several looks and saves screenshots of each to SERPIS_TEST_SHOTS, to compare: <look>-1.png
-- from the top, then -2 a page further down, so the whole report is seen. SERPIS_LOOKS picks
-- some of them; SERPIS_TEST_SIZE sets the size they are laid out at.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
local shots = os.getenv("SERPIS_TEST_SHOTS")
-- Each look: a name and the settings it changes (the rest stay at their defaults).
local looks = {
  { "serpis-dark", {}, "serpis-dark" },
  { "serpis-light", {}, "serpis-light" },
  { "github-light", {}, "github-light" },
  { "github-dark-dimmed", {}, "github-dark-dimmed" },
  { "modus-vivendi", {}, "modus-vivendi" },
  { "modus-operandi-tinted", {}, "modus-operandi-tinted" },
  { "rayfish", {}, "rayfish" },
}
-- SERPIS_LOOKS=name,name: only those looks.
local only = os.getenv("SERPIS_LOOKS")
if only then
  local keep = {}
  for _, l in ipairs(looks) do
    if ("," .. only .. ","):find("," .. l[1] .. ",", 1, true) then keep[#keep + 1] = l end
  end
  looks = keep
end
local pages = 2
local steps = {
  function() serpis.command.run("new-chat"); return 500 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "STEPS:Run the tests|echo ok;;Check the diff|echo ok"); return 2500 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "REPORT"); return 2500 end,
}
for _, look in ipairs(looks) do
  steps[#steps + 1] = function()
    for k, v in pairs(look[2]) do serpis.var.set(k, v) end
    -- A look's third field: the theme it is shown in.
    if look[3] then serpis.command.run("select-theme", look[3]) end
    return 1200
  end
  steps[#steps + 1] = function()
    serpis.command.run("chat-focus-transcript")
    serpis.command.run("beginning-of-buffer")
    return 800
  end
  for p = 1, pages do
    steps[#steps + 1] = function()
      if shots then d.screenshot(string.format("%s/%s-%d.png", shots, look[1], p), true) end
      return 700
    end
    if p < pages then
      steps[#steps + 1] = function()
        -- A page down with the wheel, over the transcript (the composer covers the bottom).
        d.mouse_move(400, 500)
        d.wheel(-850)
        return 900
      end
    end
  end
end
steps[#steps + 1] = function()
  T.check("the report came", (serpis.chat.transcript() or ""):find("Things to consider", 1, true) ~= nil, true)
  T.finish()
  return 100000
end
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return end
  local ok, wait = pcall(steps[i])
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
