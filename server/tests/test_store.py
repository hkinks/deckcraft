async def test_users_roundtrip(store):
    uid = await store.add_user("ana", "hash1", admin=True)
    u = await store.get_user("ana")
    assert u.id == uid and u.admin and not u.disabled and u.password_hash == "hash1"
    await store.set_password("ana", "hash2")
    assert (await store.get_user("ana")).password_hash == "hash2"
    await store.disable("ana")
    assert (await store.get_user("ana")).disabled
    assert await store.get_user("nobody") is None


async def test_conversations_messages_and_turns(store):
    uid = await store.add_user("bo", "h")
    cid = await store.create_conversation(uid, "deck#1", "First")
    await store.append_message(cid, "user", {"text": "hi"})
    await store.append_message(cid, "assistant", {"text": "hello"})
    msgs = await store.messages(cid)
    assert [m["role"] for m in msgs] == ["user", "assistant"]
    assert msgs[1]["content"] == {"text": "hello"}
    tid = await store.start_turn(cid, "make it red", "qwen-local")
    await store.finish_turn(tid, ["shape.fill"], [0], "ok", {"input_tokens": 5})
    turns = await store.turns(cid)
    assert turns[0]["status"] == "ok" and turns[0]["actions"] == ["shape.fill"] and turns[0]["slides"] == [0]
    convs = await store.list_conversations(uid, "deck#1")
    assert convs[0]["id"] == cid
    assert await store.list_conversations(uid, "other") == []
