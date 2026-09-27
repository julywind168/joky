-- Native Lua integer arithmetic, normalized to signed Int32 only on return.
-- These kernels use only +/* on the accumulator, so its low 32 bits match
-- Joky's wrapping Int32 result even when Lua's wider integer overflows.
local function int32(value)
    value = value & 0xffffffff
    if value >= 0x80000000 then value = value - 0x100000000 end
    return value
end

local function while_sum(n, seed)
    local i, total = 0, seed
    while i < n do
        total = total + i
        i = i + 1
    end
    return int32(total)
end

local function while_recurrence(n, seed)
    local i, value = 0, seed
    while i < n do
        value = ((value * 1664525) + 1013904223) + i
        i = i + 1
    end
    return int32(value)
end

-- Lua has no assignment expression. Preserve the condition's increment,
-- including its final failing check, with an explicit break.
local function while_sequence(n, seed)
    local checks, total = 0, seed
    while true do
        checks = checks + 1
        if checks > n then break end
        total = total + (checks - 1)
    end
    return int32(total)
end

-- A second variant explicitly keeps the accumulator's low 32 bits each
-- iteration. This adds one mask per body; it is not native Lua arithmetic.
-- Signed intermediate conversion is unnecessary for these kernels because
-- neither branches nor division observe the accumulator inside the loop.
local function while_sum_mask32(n, seed)
    local i, total = 0, seed
    while i < n do
        total = (total + i) & 0xffffffff
        i = i + 1
    end
    return int32(total)
end

local function while_recurrence_mask32(n, seed)
    local i, value = 0, seed
    while i < n do
        value = (((value * 1664525) + 1013904223) + i) & 0xffffffff
        i = i + 1
    end
    return int32(value)
end

local function while_sequence_mask32(n, seed)
    local checks, total = 0, seed
    while true do
        checks = checks + 1
        if checks > n then break end
        total = (total + (checks - 1)) & 0xffffffff
    end
    return int32(total)
end

return while_sum, while_recurrence, while_sequence,
       while_sum_mask32, while_recurrence_mask32, while_sequence_mask32
