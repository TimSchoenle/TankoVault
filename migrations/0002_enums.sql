-- Domain enum types. Tokens are mirrored 1:1 by `tankovault_domain::enums`.
CREATE TYPE content_type   AS ENUM ('manga','manhwa','manhua','webtoon','unknown');
CREATE TYPE series_status  AS ENUM ('ongoing','completed','hiatus','cancelled','unknown');
CREATE TYPE adapter_kind   AS ENUM ('madara','generic_config','custom');
CREATE TYPE provider_state AS ENUM ('active','degraded','challenged','solving','blocked','disabled');
CREATE TYPE scan_mode      AS ENUM ('full','fast');
CREATE TYPE run_state      AS ENUM ('queued','running','completed','failed','cancelled');
CREATE TYPE task_state     AS ENUM ('queued','claimed','running','done','failed','skipped');
CREATE TYPE watch_status   AS ENUM ('reading','planned','completed','dropped','paused');
CREATE TYPE user_role      AS ENUM ('user','operator','admin');
