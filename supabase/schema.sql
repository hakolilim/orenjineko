-- Schema cho bot nối từ PhoBo (Supabase / Postgres)
-- Chạy 1 lần trong Supabase SQL Editor.
-- SUPABASE_KEY dùng Secret key hoặc khóa service_role cho tiến trình bot.
-- Database đã có sẵn: chạy khối increment_stat ở cuối file TRƯỚC khi chạy bản bot mới
-- (bot dùng hàm này để cộng dồn bộ đếm global_stats nguyên tử khi có nhiều shard).

create table if not exists words (
    word text primary key
);

create table if not exists report_words (
    word text primary key
);

-- Mỗi hàng = 1 kênh nối từ. Một guild có thể có nhiều kênh active đồng thời.
create table if not exists guild_config (
    channel_id text primary key,
    guild_id   text not null,
    bot_mode   boolean not null default false
);

create index if not exists guild_config_guild_idx on guild_config (guild_id);

-- Nếu bảng đã tồn tại từ trước, thêm cột bot_mode:
alter table guild_config add column if not exists bot_mode boolean not null default false;

-- Migration từ mô hình cũ (PK = guild_id, 1 kênh/guild) sang mới (PK = channel_id):
-- Chạy 1 lần nếu bảng cũ vẫn còn PK trên guild_id.
-- alter table guild_config drop constraint guild_config_pkey;
-- alter table guild_config add primary key (channel_id);

create table if not exists game_state (
    channel_id          text primary key,
    running             boolean not null default false,
    current_player_id   text,
    current_player_name text,
    words               jsonb not null default '[]'::jsonb
);

-- Kênh đếm số: cấu hình và số hiện tại được lưu cùng nhau, không lưu xếp hạng.
-- Dùng text + số thập phân trong Rust để số lớn không bị làm tròn khi truyền JSON.
create table if not exists counting_channels (
    channel_id    text primary key,
    guild_id      text not null,
    last_player_id text,
    current_count text not null default '0'
        check (current_count ~ '^(0|[1-9][0-9]*)$')
);

-- Nâng cấp kênh đếm số đã tồn tại để giữ luật đổi người khi khởi động lại.
alter table counting_channels add column if not exists last_player_id text;

create table if not exists rankings (
    guild_id   text not null,
    user_id    text not null,
    win        integer not null default 0,
    total      integer not null default 0,
    true_count integer not null default 0,
    name       text,
    avatar     text,
    primary key (guild_id, user_id)
);

create index if not exists rankings_guild_idx on rankings (guild_id);

create table if not exists global_stats (
    key   text primary key,
    value bigint not null default 0
);

insert into global_stats (key, value) values
    ('query', 0),
    ('word_played', 0),
    ('round_played', 0)
on conflict (key) do nothing;

create table if not exists premium_guilds (
    guild_id text primary key
);

-- Quyền Data API cho bot. Với database đã có bảng, có thể chạy riêng phần này.
grant usage on schema public to service_role;

grant select, insert, update on table
    public.words, public.global_stats
to service_role;

grant select, insert, update, delete on table
    public.report_words, public.guild_config, public.game_state,
    public.counting_channels, public.rankings
to service_role;

grant select, delete on table public.premium_guilds to service_role;

-- Cộng dồn nguyên tử bộ đếm global_stats và trả về giá trị mới. Bot gọi qua RPC
-- (POST /rest/v1/rpc/increment_stat) để nhiều tiến trình/shard không ghi đè số của nhau.
-- Chạy độc lập được trên database cũ. Chỉ service_role được gọi.
create or replace function public.increment_stat(stat_key text, delta bigint)
returns bigint
language plpgsql
set search_path = public
as $$
declare
    result bigint;
begin
    if delta < 0 then
        raise exception 'delta phải >= 0' using errcode = '22023';
    end if;
    insert into public.global_stats as s (key, value) values (stat_key, delta)
    on conflict (key) do update set value = s.value + excluded.value
    returning s.value into result;
    return result;
end
$$;

revoke all on function public.increment_stat(text, bigint) from public, anon, authenticated;
grant execute on function public.increment_stat(text, bigint) to service_role;

notify pgrst, 'reload schema';
