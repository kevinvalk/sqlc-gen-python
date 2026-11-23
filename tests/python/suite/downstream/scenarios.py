"""Populated database checks for the pinned external sqlc projects."""

from __future__ import annotations

import datetime as dt
from decimal import Decimal
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from suite.downstream.replay import Queries


async def accounts(queries: Queries) -> None:
    await queries.call("create_user", email="sample@example.test", type="member", nickname="first")
    ident = queries.sql("SELECT id FROM users WHERE email='sample@example.test'")[0][0]
    assert (await queries.call("get_user", id=ident)).nickname == "first"
    await queries.call("update_nickname", id=ident, nickname=None)
    assert (await queries.call("get_user", id=ident)).nickname is None
    assert len(await queries.call("find_user_by_email", lower="SAMPLE@EXAMPLE.TEST")) == 1


async def chocode(queries: Queries) -> None:
    assert await queries.call("get_user_by_name", username="alice") is None
    value = "fixture text"
    await queries.call("create_user", username="alice", password=value, role="author")
    row = await queries.call("get_user_by_name", username="alice")
    assert (row.username, row.role) == ("alice", "author")
    assert await queries.call("get_user", id=row.id) == row
    assert queries.sql("SELECT username,role FROM users") == [("alice", "author")]


async def bank(queries: Queries) -> None:
    async with queries.rollback():
        result = await queries.call("create_user", name="Temporary")
        await queries.call("create_user_detail", id=result.lastrowid, email="rollback@example.test")
        assert queries.sql("SELECT COUNT(*) FROM users WHERE name='Temporary'") == [(0,)]
    assert queries.sql("SELECT COUNT(*) FROM user_details WHERE email='rollback@example.test'") == [(0,)]
    await queries.call("create_user", name="Ada")
    ident = queries.sql("SELECT id FROM users WHERE name='Ada'")[0][0]
    await queries.call("create_user_detail", id=ident, email="ada@example.test")
    assert queries.sql("SELECT name,email FROM users JOIN user_details USING (id)") == [("Ada", "ada@example.test")]


async def login(queries: Queries) -> None:
    value = "fixture hash"
    await queries.call("create_user", email="new@example.test", password_hash=value)
    row = await queries.call("get_user_by_email", email="new@example.test")
    assert row.password_hash == value
    assert row in await queries.call("list_users")
    assert await queries.call("get_user_by_email", email="absent@example.test") is None
    assert queries.sql("SELECT password_hash FROM users WHERE email='new@example.test'") == [(value,)]


async def ledger(queries: Queries) -> None:
    date = dt.date(2024, 2, 29)
    await queries.call("create_event", dt_=date, money=-125, description="train")
    row = await queries.call("get_event", dt_=date, money=-125, description="train")
    assert row.dt_ == date
    assert row.money == -125
    await queries.call("create_tag", name="travel")
    tag = await queries.call("get_tag", name="travel")
    await queries.call("map_event_to_tag", event_id=row.id, tag_id=tag.id)
    rows = await queries.call("list_events_with_tags", from_dt=date, to_dt=date, tagnames=["travel", "other"])
    assert len(rows) == 1
    assert rows[0].money == -125
    await queries.call("update_event_money", money=17, id=row.id)
    assert (await queries.call("get_event_by_id", id=row.id)).money == 17
    await queries.call("delete_event_by_id", id=row.id)
    assert queries.sql("SELECT COUNT(*) FROM event_to_tag") == [(0,)]


async def events(queries: Queries) -> None:
    ident = await queries.call("create_step", workflow_name="workflow", instance_id="one", step_name="step")
    await queries.call(
        "create_id_command",
        enrollment_id="device",
        command_uuid="cmd",
        step_id=ident,
        request_type="Inventory",
    )
    assert await queries.call("get_request_type", enrollment_id="device", command_uuid="cmd") == "Inventory"
    assert await queries.call("get_step_i_ds_by_enrollment_id", enrollment_id="device") == [ident]
    await queries.call(
        "delete_id_commands_by_workflow_name",
        enrollment_id="device",
        workflow_name="workflow",
    )
    assert await queries.call("get_request_type", enrollment_id="device", command_uuid="cmd") is None
    assert queries.sql("SELECT COUNT(*) FROM id_commands") == [(0,)]


async def profiles(queries: Queries) -> None:
    queries.sql(
        "INSERT INTO subsystem_profiles (name,profile_id,profile_uuid,raw_profile) VALUES ('a','id1','uuid1','xml-a'),('b','id2','uuid2','xml-b')"
    )
    assert sorted(r.name for r in await queries.call("get_all_profile_infos")) == ["a", "b"]
    for names, expected in [
        ([], []),
        (["a"], ["a"]),
        (["b", "a", "missing"], ["a", "b"]),
    ]:
        assert sorted(r.name for r in await queries.call("get_profile_infos", names=names)) == expected
    assert [r.raw_profile for r in await queries.call("get_raw_profiles", names=["b"])] == ["xml-b"]
    await queries.call("delete_profile", name="a")
    assert queries.sql("SELECT name FROM subsystem_profiles") == [("b",)]


async def people(queries: Queries) -> None:
    await queries.call("create", first_name="Lee", last_name=None)
    (row,) = await queries.call("list_")
    assert row.last_name is None
    assert row.first_name == "Lee"
    await queries.call("update", id=row.id, first_name="Ada", last_name="Lovelace")
    (found,) = await queries.call("search", first_name="A%", last_name="L%")
    assert found.id == row.id
    assert (await queries.call("get", id=row.id)).last_name == "Lovelace"
    await queries.call("delete", id=row.id)
    assert await queries.call("get", id=row.id) is None
    assert queries.sql("SELECT COUNT(*) FROM person") == [(0,)]


async def pets(queries: Queries) -> None:
    await queries.call("create_owner", name="Jo", bio=None)
    (owner,) = await queries.call("list_owners")
    assert owner.total_pets == 0
    assert owner.bio is None
    for name in ["Milo", "Mo"]:
        await queries.call("create_owner_pet", name=name, type="cat", owner_id=owner.id)
    assert (await queries.call("get_owner", id=owner.id)).total_pets == 2
    pets = await queries.call("list_owner_pets", owner_id=owner.id)
    assert [r.name for r in pets] == ["Milo", "Mo"]
    assert (await queries.call("get_owner_pet", id=pets[0].id, owner_id=owner.id)).name == "Milo"
    await queries.call("delete_owner_pet", owner_id=owner.id, id=pets[0].id)
    await queries.call("delete_owner", id=owner.id)
    assert queries.sql("SELECT COUNT(*) FROM pets") == [(0,)]


async def products(queries: Queries) -> None:
    await queries.call("create_product", name="Coffee", description="dark", price=299)
    (row,) = await queries.call("list_products")
    assert (row.name, row.price) == ("Coffee", 299)
    await queries.call("update_product", id=row.id, name="Tea", description="light", price=199)
    assert (await queries.call("get_product_by_id", id=row.id)).price == 199
    assert queries.sql("SELECT name,price FROM products") == [("Tea", 199)]
    await queries.call("delete_product", id=row.id)
    assert await queries.call("get_product_by_id", id=row.id) is None


async def todos(queries: Queries) -> None:
    key = "todo-01"
    await queries.call("add_todo", uuid=key, value_="buy coffee")
    assert (await queries.call("get_todo", uuid=key)).done_at is None
    await queries.call("toggle_todo", uuid=key)
    assert (await queries.call("get_todo", uuid=key)).done_at is not None
    await queries.call("edit_todo", uuid=key, value_="buy tea")
    assert [row.value for row in await queries.call("list_todos", search="%tea%", limit=1, offset=0)] == ["buy tea"]
    await queries.call("archive_todo", uuid=key)
    assert await queries.call("total_todos", search="%") == 0
    assert await queries.call("total_archived_todos", search="%") == 1
    assert [row.uuid for row in await queries.call("list_archived_todos", search="%", limit=1, offset=0)] == [key]
    await queries.call("restore_todo", uuid=key)
    assert queries.sql("SELECT value,archived_at FROM todo") == [("buy tea", None)]


async def bookings(queries: Queries) -> None:
    when = dt.datetime(2025, 6, 20, 13, 0)  # noqa: DTZ001 -- the MySQL driver binds a local datetime.
    ident = await queries.call(
        "create_booking",
        user_id="ada",
        course_id=8,
        golfers=2,
        datetime=when,
        price=12.5,
        booking_id="book1",
        stripe_payment_id="pay1",
    )
    row = await queries.call("get_a_booking", id=ident)
    assert row.golfers == 2
    assert row.datetime == when
    await queries.call("update_and_confirm", stripe_payment_id="paid", id=ident)
    rows = await queries.call("get_confirmed_bookings_by_date_and_course", course_id=8, datetime=when)
    # MySQL finds no booking when an afternoon datetime is compared with `DATE(datetime)` and MariaDB finds it, so we
    # take what to expect from the same statement run directly.
    expected = queries.sql(
        "SELECT id FROM bookings WHERE status='confirmed' AND course_id=8 AND DATE(datetime)=%s",
        (when,),
    )
    assert [(r.id,) for r in rows] == expected
    midnight = when.replace(hour=0)
    assert [
        r.id for r in await queries.call("get_confirmed_bookings_by_date_and_course", course_id=8, datetime=midnight)
    ] == [ident]
    assert queries.sql("SELECT status FROM bookings") == [("confirmed",)]


async def dep(queries: Queries) -> None:
    assert await queries.call("get_syncer_cursor", name="school") is None
    for value in ["first", "last"]:
        await queries.call("store_cursor", name="school", syncer_cursor=value)
        assert await queries.call("get_syncer_cursor", name="school") == value
    await queries.call("store_config", name="school", config_base_url="https://example.test/")
    assert await queries.call("get_config_base_url", name="school") == "https://example.test/"
    assert queries.sql("SELECT syncer_cursor FROM dep_names WHERE name='school'") == [("last",)]


async def schedule(queries: Queries) -> None:
    ident = await queries.call(
        "create_task",
        url="/jobs",
        method="POST",
        namespace="daily",
        params={"x": [1]},
        headers={},
        body={"ok": True},
        start_unix=10,
        end_unix=20,
        interval="10s",
        paused=False,
    )
    row = await queries.call("get_task_by_id", id=ident)
    assert row.params == {"x": [1]}
    assert row.paused is False
    assert len(await queries.call("get_tasks_by_namespace", namespace="daily")) == 1
    await queries.call("update_task_status", id=ident, paused=True)
    assert await queries.call("get_active_tasks", end_unix=10) == []
    assert queries.sql("SELECT paused, body->>'ok' FROM tasks") == [(True, "true")]
    await queries.call("delete_task", id=ident)
    assert await queries.call("get_tasks") == []


async def global_settings(queries: Queries) -> None:
    key = await queries.call(
        "insert_global",
        name="region",
        description="",
        value_="west",
        created_at="2025-01-01",
        last_used_at="2025-02-01",
    )
    assert (await queries.call("get_last_used_global_by_name", name="region")).value == "west"
    row = await queries.call("update_global_last_used_at", id=key, last_used_at="2025-03-01")
    assert row.last_used_at == "2025-03-01"
    assert await queries.call("get_globals_by_name", name="region") == [row]
    assert (await queries.call("delete_global", id=key)).id == key
    assert queries.sql("SELECT COUNT(*) FROM globals") == [(0,)]


async def social(queries: Queries) -> None:
    queries.sql("INSERT INTO members (id,name,handle) VALUES ('u','Jo','jo')")
    await queries.call(
        "insert_entry",
        id="p",
        author_id="u",
        body="bonjour",
        topic="design",
        created_at="2025-01-01",
        request_id="r",
    )
    row = await queries.call("get_post", id="p", viewer_id="u")
    assert row.likes == 0
    assert row.liked is False
    await queries.call("insert_reaction", entry_id="p", member_id="u")
    row = await queries.call("get_post", id="p", viewer_id="u")
    assert row.likes == 1
    assert row.liked is True
    assert [
        p.id for p in await queries.call("list_posts", viewer_id="u", topic="design", needle="", offset=0, limit=2)
    ] == ["p"]
    await queries.call("delete_reaction", entry_id="p", member_id="u")
    assert queries.sql("SELECT COUNT(*) FROM reactions") == [(0,)]


async def characters(queries: Queries) -> None:
    await queries.call("upsert_character", id="a", name="Mio", multi_voice=1)
    assert (await queries.call("get_character", id="a")).multi_voice == 1
    await queries.call("upsert_character", id="a", name="Miyu", multi_voice=0)
    assert [r.name for r in await queries.call("list_characters")] == ["Miyu"]
    await queries.call("delete_character", id="a")
    assert await queries.call("get_character", id="a") is None
    assert queries.sql("SELECT COUNT(*) FROM characters") == [(0,)]


async def broadcast(queries: Queries) -> None:
    keys = {"channel_type": "tv", "channel_id": "x", "service_id": 5}
    for value in [b"\x00\xff", b"change"]:
        await queries.call("upsert_snapshot", **keys, pmt_section=value, stored_at=20)
        row = await queries.call("get_snapshot", **keys)
        assert row.pmt_section == value
        assert row.stored_at == 20
    await queries.call("delete_expired_snapshots", stored_at=25)
    assert await queries.call("get_snapshot", **keys) is None
    assert queries.sql("SELECT COUNT(*) FROM data_broadcast_snapshots") == [(0,)]


async def blog(queries: Queries) -> None:
    await queries.call("create_post", title="audit post", content="body", views=0)
    ident = queries.sql("SELECT id FROM iris_posts WHERE title='audit post'")[0][0]
    await queries.call("view_plus", id=ident)
    assert (await queries.call("get_post", id=ident)).views == 1
    await queries.call("delete_post", id=ident)
    assert await queries.call("get_post", id=ident) is None
    assert queries.sql("SELECT COUNT(*) FROM iris_posts WHERE title='audit post'") == [(0,)]


async def rankings(queries: Queries) -> None:
    queries.sql(
        "INSERT INTO users (id,email,name,password_hash) VALUES ('u','u@example.test','Uma','x'),('v','v@example.test','Vic','x')"
    )
    queries.sql("INSERT INTO teams (id,name,created_by) VALUES ('g','Lab','u')")
    queries.sql(
        "INSERT INTO tasks (id,title,team_id,created_by) VALUES ('1','a','g','u'),('2','b','g','u'),('3','c','g','v')"
    )
    rows = await queries.call("top_creators_by_team", team_id="g")
    assert [(r.user_name, r.tasks_created, r.rn) for r in rows] == [
        ("Uma", 2, 1),
        ("Vic", 1, 2),
    ]
    assert queries.sql("SELECT created_by,COUNT(*) FROM tasks GROUP BY created_by ORDER BY COUNT(*) DESC") == [
        ("u", 2),
        ("v", 1),
    ]


async def courses(queries: Queries) -> None:
    latitude = Decimal("12.345678")
    ident = await queries.call(
        "create_course",
        name="West",
        holes=18,
        lapsed=False,
        live=True,
        street_address_1="1 Example",
        city="Utrecht",
        state="UT",
        postal_code="1000",
        country="NL",
        latitude=latitude,
    )
    row = await queries.call("get_a_course", id=ident)
    assert row.latitude == latitude
    assert row.longitude is None
    assert row.live is True
    assert await queries.call("list_courses", limit=2, offset=0) == [row]
    assert queries.sql("SELECT latitude,longitude FROM courses") == [(latitude, None)]


async def languages(queries: Queries) -> None:
    for ident, name in [(123, "one"), (124, "two")]:
        await queries.call("seed_language", id=ident, name=name, display_name=name)
    await queries.call("delete_languages", ids=[])
    assert queries.sql("SELECT id FROM language WHERE id IN (123,124) ORDER BY id") == [
        (123,),
        (124,),
    ]
    await queries.call("delete_languages", ids=[124, 123])
    assert queries.sql("SELECT COUNT(*) FROM language WHERE id IN (123,124)") == [(0,)]
