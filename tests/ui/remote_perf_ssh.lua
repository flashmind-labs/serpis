-- Not part of the suite: an empty chat on a real machine, in its home, idle for a while (to
-- sample). SERPIS_TEST_REMOTE_HOST names it; the chat is closed there afterwards.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("remote-offer-setup", false)
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-ssh-args", { "-o", "StrictHostKeyChecking=accept-new" })
serpis.var.set("remote-hosts", { { name = "box", host = os.getenv("SERPIS_TEST_REMOTE_HOST") } })
serpis.var.set("remote-node-builds", os.getenv("SERPIS_TEST_NODE_BUILDS"))
local f0
serpis.command.run("remote-new-chat", "box", "~")
local function wait()
  if serpis.chat.info() and serpis.chat.info().machine == "box" and (serpis.chat.info().cwd or "") ~= "" then
    print("READY " .. serpis.chat.info().cwd)
    io.open("/tmp/snperf-ready", "w"):close()
    f0 = d.frames()
    serpis.defer(8000, function()
      local t = d.frame_times()
      local slow = 0
      for _, x in ipairs(t) do if x > 50 then slow = slow + 1 end end
      T.check(string.format("cwd=%s frames=%d slow=%d last=%.1fms", serpis.chat.info().cwd, d.frames() - f0, slow, t[#t]), true, true)
      serpis.command.run("remote-close-chat")
      serpis.defer(1000, function() T.finish() end)
    end)
  else
    serpis.defer(200, wait)
  end
end
wait()
