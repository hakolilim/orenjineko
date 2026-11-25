# OrenjiNeko

Bot Discord tiếng Việt với trò chơi nối từ, đếm số và trò chuyện cùng AI. Dữ liệu trò chơi được lưu trên **Supabase (Postgres)**, có cache RAM để xử lý lượt chơi nhanh và khôi phục trạng thái khi khởi động lại.

[Mã nguồn](https://github.com/hakolilim/orenjineko) · [Support Discord Server](https://discord.gg/NC7Ks2c5wE) · [Privacy Policy](PRIVACY_POLICY.md) · [Terms of Service](TERMS_OF_SERVICE.md)

Người duy trì: **hakolilim**. Để được hỗ trợ, báo lỗi hoặc yêu cầu truy cập, sửa, xoá dữ liệu, hãy liên hệ trực tiếp người duy trì tại server hỗ trợ.

## Tính năng

- Nối từ tiếng Việt giữa các thành viên hoặc chơi với bot; hỗ trợ nhiều kênh trong cùng máy chủ.
- Đếm số luân phiên giữa các thành viên, lưu số hiện tại và người đếm gần nhất.
- Bảng xếp hạng nối từ theo máy chủ, thống kê cá nhân và thống kê toàn bot.
- Báo cáo từ không phù hợp, đề xuất từ mới và quản lý danh sách đen qua kênh duyệt.
- Chat AI qua API tương thích OpenAI Chat Completions, có streaming và hỗ trợ ngữ cảnh hình ảnh.
- Trang trạng thái, gợi ý nối từ, API trạng thái và hai trang chính sách công khai.

## Cài đặt và chạy

### Yêu cầu

- **Rust và Cargo 1.88+** trên Linux, cùng libcurl 4, OpenSSL 3, Cairo 2, font DejaVu Sans và bộ chứng chỉ CA.
- Một ứng dụng bot Discord và bot token.
- Một project Supabase với URL và khoá `service_role`.
- Kết nối tới Discord, Supabase và GitHub khi khởi tạo từ điển lần đầu; thêm kết nối tới nhà cung cấp AI nếu bật `/ai`.

### Chuẩn bị Discord

Tạo ứng dụng bot của riêng bạn trong [Discord Developer Portal](https://discord.com/developers/applications), lấy bot token và bật **Message Content Intent**. Mã nguồn sử dụng các intent `Guilds`, `GuildMessages` và `MessageContent`.

Mời bot vào máy chủ với các scope `bot` và `applications.commands`. Cấp quyền **View Channel**, **Send Messages**, **Embed Links** và **Add Reactions**. Dùng thêm **Read Message History** cho đếm số và ngữ cảnh AI, cùng **Attach Files** cho ảnh công thức trong câu trả lời AI. Bot cần truy cập được kênh trò chơi và kênh báo cáo nếu sử dụng tính năng duyệt từ.

### Cài mã nguồn và cơ sở dữ liệu

Trên Ubuntu 24.04, cài thư viện hệ thống nếu máy chưa có:

```bash
sudo apt install libcurl4t64 libssl3t64 libcairo2 fonts-dejavu-core ca-certificates
```

Chạy các lệnh sau từ thư mục chứa `Cargo.toml`:

```bash
cp .env.example .env
cargo build --release --locked --offline
```

Điền `BOT_TOKEN`, `SUPABASE_URL` và `SUPABASE_KEY` vào `.env`. Nếu đã triển khai Phobophake, dùng bộ biến môi trường và cơ sở dữ liệu hiện tại. Bản Rust đọc cùng các bảng, khoá và kiểu dữ liệu. Với cơ sở dữ liệu mới, mở Supabase SQL Editor và chạy [schema.sql](supabase/schema.sql). Schema có ghi chú cho các bản Supabase cũ còn dùng khoá chính `guild_id` trên `guild_config`.

```bash
./target/release/orenjineko
```

Bot nạp dữ liệu từ Supabase, mở web server rồi kết nối Discord và đồng bộ slash command. API trạng thái hiển thị `booting` cho đến khi Gateway sẵn sàng. Nếu bảng `words` rỗng, bot tải và seed từ điển từ GitHub. Các lần sau bot nạp từ Supabase.

Nếu Data API trả `permission denied for table words`, kiểm tra `SUPABASE_URL` và `SUPABASE_KEY` thuộc cùng project. Lấy **Secret key** (`sb_secret_…`) hoặc khóa **`service_role`** cũ trong **Settings → API Keys**. Biến được export trong shell có ưu tiên cao hơn `.env`. Nếu khóa đúng và lỗi vẫn còn, chạy các lệnh `GRANT` cuối [schema.sql](supabase/schema.sql) trong Supabase SQL Editor. Các lệnh cấp quyền cho vai trò `service_role` trên các bảng bot dùng. [Tài liệu Supabase về quyền Data API](https://supabase.com/kb/troubleshooting/database-api-42501-errors)

## Biến môi trường

Tham khảo [`.env.example`](.env.example). Không cần cấu hình AI để sử dụng các trò chơi.

| Biến | Ý nghĩa / mặc định |
| --- | --- |
| `BOT_TOKEN` | Token bot Discord, bắt buộc. |
| `SUPABASE_URL` | URL project Supabase, bắt buộc. |
| `SUPABASE_KEY` | Secret key (`sb_secret_…`) hoặc khóa `service_role` của Supabase, bắt buộc. |
| `REPORT_CHANNEL` | ID kênh duyệt từ và công cụ vận hành; tuỳ chọn. |
| `CORRECT_EMOJI` | Emoji phản hồi lượt đúng; mặc định `✅`. |
| `WRONG_EMOJI` | Emoji phản hồi lượt sai; mặc định `❌`. |
| `PORT` | Cổng HTTP; mặc định `3000`, lắng nghe trên `0.0.0.0`. |
| `AI_BASE_URL` | Base URL API tương thích OpenAI; mặc định `https://api.openai.com/v1`. |
| `AI_API_KEY` | Khoá API của nhà cung cấp; cần khi dùng OpenAI, có thể bỏ trống nếu API local không yêu cầu xác thực. |
| `AI_MODEL` | Tên model; mặc định `gpt-4o-mini`. |
| `AI_SYSTEM_PROMPT` | Chỉ dẫn hệ thống mặc định; nếu bỏ trống, dùng trợ lý tiếng Việt trả lời ngắn gọn. |
| `AI_TIMEOUT_MS` | Timeout gọi AI tính bằng mili giây; mặc định `30000`, giá trị rỗng hoặc không phải số dương sẽ dùng mặc định. |

`/ai` chỉ được bật khi `AI_BASE_URL` hoặc `AI_API_KEY` có giá trị. Base URL là địa chỉ trước `/chat/completions`, không phải URL endpoint đầy đủ; dấu `/` cuối được tự loại bỏ.

Ví dụ cấu hình AI qua OpenAI:

```dotenv
AI_BASE_URL=https://api.openai.com/v1
AI_API_KEY=your_api_key
AI_MODEL=gpt-4o-mini
AI_TIMEOUT_MS=30000
```

Có thể dùng endpoint tương thích từ provider khác, proxy hoặc model local; đặt `AI_BASE_URL` và `AI_MODEL` theo dịch vụ đó.

## Các lệnh

Các ví dụ dùng tên tham số của slash command; với tham số có lựa chọn, chọn giá trị tương ứng trong giao diện Discord.

| Lệnh | Chức năng | Điều kiện |
| --- | --- | --- |
| `/help` | Hướng dẫn sử dụng OrenjiNeko. | Thành viên sử dụng được lệnh. |
| `/set-channel channel:#kênh [bot_mode]` | Chọn kênh và chế độ nối từ hoặc đếm số. | `MANAGE_GUILD`. |
| `/unset-channel channel:#kênh` | Xoá cấu hình và trạng thái trò chơi của kênh. | `MANAGE_GUILD`. |
| `!start` | Bắt đầu ván nối từ nếu chưa có ván. | Gửi được tin nhắn trong kênh nối từ. |
| `!stop` | Kết thúc ván nối từ đang chạy và bắt đầu ván mới. | Gửi được tin nhắn trong kênh nối từ. |
| `/rank` | Bảng xếp hạng nối từ của máy chủ. | Dùng trong máy chủ. |
| `/me` | Thống kê nối từ cá nhân trong máy chủ. | Dùng trong máy chủ. |
| `/stats` | Thống kê toàn bot. | Thành viên sử dụng được lệnh. |
| `/server` | Thông tin máy chủ và trạng thái Premium. | Dùng trong máy chủ. |
| `/ping` | Độ trễ phản hồi và WebSocket. | Thành viên sử dụng được lệnh. |
| `/report word:<từ> [reason:<lý do>] [type]` | Báo cáo từ (`report`, mặc định) hoặc đề xuất từ mới (`add`). | Đã cấu hình `REPORT_CHANNEL`. |
| `/unblacklist word:<từ> [action]` | Kiểm tra (`check`, mặc định) hoặc gỡ từ khỏi danh sách đen (`remove`). | Trong `REPORT_CHANNEL`; thao tác gỡ cần `MANAGE_GUILD`. |
| `/broadcast message:<nội dung>` | Gửi thông báo đến các kênh nối từ và đếm số đã cấu hình. | Trong `REPORT_CHANNEL`; mọi thành viên có quyền dùng lệnh tại đó đều dùng được. |
| `/leave [guild:<máy chủ>]` | Xoá dữ liệu của máy chủ được chọn rồi cho bot rời. | `MANAGE_GUILD`; tuỳ chọn `guild` chỉ dùng trong `REPORT_CHANNEL`. |
| `/ai message:<câu hỏi> [system:<chỉ dẫn>] [visible:false]` | Chat AI với ngữ cảnh từ kênh hiện tại. | Đã cấu hình AI; bot đọc được lịch sử kênh. |

Có thể dùng `?phobo set` để chọn **kênh hiện tại** làm kênh nối từ, hoặc `?phobo unset` để tắt trò chơi trong kênh hiện tại. Cả hai cần `MANAGE_GUILD`; đây là prefix được giữ trong mã nguồn.

### Nối từ

Dùng `/set-channel` và chọn **Nối từ (False)**, hoặc bỏ trống `bot_mode`. Chọn **Nối từ với bot (True)** để bot tự nối tiếp sau mỗi lượt hợp lệ của thành viên, rồi gõ `!start` trong kênh chơi.

Mỗi lượt gửi một cụm đúng **2 tiếng**, tiếng đầu trùng tiếng cuối của từ trước. Ví dụ: `học sinh` → `sinh nhật` → `nhật ký`. Không được đi hai lượt liên tiếp hoặc dùng lại từ trong cùng ván. Từ phải có trong từ điển và không thuộc danh sách đen. Nếu không còn từ hợp lệ để nối, người đi cuối thắng và bot tự bắt đầu ván mới.

`!stop` đặt lại ván đang chơi và mở ván mới. Theo logic hiện tại, lệnh này **không yêu cầu quyền quản trị**; thành viên gửi được tin nhắn trong kênh đều có thể dùng.

### Đếm số

Dùng `/set-channel` và chọn **Đếm số** trong `bot_mode`. Không cần `!start`. Gửi số nguyên tăng dần `1`, `2`, `3`… và đổi người sau mỗi lượt đúng.

Đếm sai, gửi số âm hoặc thập phân, hoặc một người đếm hai lần liên tiếp sẽ đặt lại số về **0**. Sau khi đặt lại, bất kỳ ai cũng có thể gửi `1`. Tin nhắn không phải số được bỏ qua. Trò chơi không có xếp hạng và không cộng thống kê nối từ.

Số hiện tại và người đếm gần nhất được lưu ngay vào Supabase, nạp lại khi khởi động. Chọn lại **Đếm số** giữ nguyên số đang đếm. `/unset-channel` xoá cấu hình và số đã lưu. Chọn một chế độ khác bằng `/set-channel` thay thế trò chơi đang được cấu hình trong kênh.

### Báo cáo từ và công cụ vận hành

`/report` nhận cụm đúng 2 tiếng. Loại `report` dùng cho từ đã có trong từ điển; `add` dùng để đề xuất từ chưa có. Bot gửi từ, lý do và thông tin người gửi/máy chủ tới `REPORT_CHANNEL` để duyệt; từ được chấp nhận cập nhật từ điển hoặc danh sách đen dùng chung. Bot có thể nhắn riêng kết quả cho người gửi.

Các phiếu còn chờ từ bản cũ vẫn duyệt được. Bot gửi DM kết quả cho các phiếu tạo từ bản Rust vì nút duyệt của các phiếu này lưu ID người gửi.

`/broadcast` gửi cùng nội dung tới tất cả kênh trò chơi đã cấu hình, loại trùng theo channel ID và không phát ping user, role hay `@everyone`. Kết quả gửi thành công/thất bại chỉ hiển thị cho người gọi. Vì mọi thành viên trong `REPORT_CHANNEL` có thể dùng công cụ này, hãy cấp quyền truy cập kênh phù hợp.

### Rời máy chủ và xoá dữ liệu

`/leave` không có tuỳ chọn sẽ xử lý máy chủ hiện tại. Trong `REPORT_CHANNEL`, tuỳ chọn `guild` cho phép chọn máy chủ từ autocomplete, nhập ID Discord 17–20 chữ số hoặc tên đầy đủ duy nhất. Tên trùng phải chọn gợi ý có ID hoặc nhập ID. Quyền `MANAGE_GUILD` được kiểm tra tại máy chủ gọi lệnh; bot phải truy cập được máy chủ đích.

Bot chặn lượt mới của máy chủ đích, chờ các lượt đang xử lý xong rồi xoá cấu hình kênh, trạng thái nối từ/đếm số, bảng xếp hạng và Premium trước khi rời. Nếu xoá thất bại, bot chưa rời; có thể gọi lại với cùng máy chủ để hoàn tất. Từ điển, danh sách đen và thống kê toàn cục không bị xoá. Kick bot trực tiếp không kích hoạt quy trình xoá này; `/unset-channel` cũng không xoá bảng xếp hạng của máy chủ.

### Trò chuyện với AI

`/ai` gửi câu hỏi, chỉ dẫn hệ thống và ngữ cảnh từ tối đa **10 tin nhắn gần nhất** trước lệnh trong kênh tới nhà cung cấp AI. Ngữ cảnh gồm tên hiển thị và có thể chứa tin nhắn của thành viên khác. Bot gửi tối đa **4 ảnh mới nhất** qua URL từ attachment, embed hoặc link preview.

Câu hỏi và câu trả lời **công khai mặc định**. Chọn `visible:false` để chỉ người gọi thấy phản hồi trên Discord; tuỳ chọn này vẫn gửi cùng ngữ cảnh tới provider. Không đưa dữ liệu nhạy cảm hoặc nội dung không có quyền chia sẻ vào câu hỏi và ngữ cảnh.

Câu trả lời được cập nhật dần khi streaming và tách thành nhiều tin nhắn nếu dài. Provider trả kết quả không streaming vẫn được hỗ trợ; một số lỗi do model không nhận ảnh sẽ được thử lại bằng văn bản kèm thông báo. Nội dung AI có thể sai, cần kiểm tra trước khi sử dụng. Chi tiết xử lý và lưu giữ dữ liệu xem [Privacy Policy](PRIVACY_POLICY.md).

Khi câu trả lời có LaTeX, bot tự gửi thêm **ảnh preview công thức** sau khi streaming kết thúc, đồng thời giữ nguyên văn bản gốc để sao chép. Hỗ trợ `$...$`, `$$...$$`, `\(...\)`, `\[...\]` và code block `latex`/`math`; bỏ qua code block thông thường, inline code và công thức trùng. Bot dựng bố cục LaTeX trong Rust và xuất PNG bằng Cairo. Preview hỗ trợ phân số, căn, chỉ số, ma trận, các nhóm công thức phổ biến, ký hiệu toán và macro theo từng công thức. Bộ dựng ảnh hỗ trợ một tập con cú pháp TeX. `visible:false` cũng áp dụng cho ảnh. Mỗi câu trả lời preview tối đa **12 công thức**, mỗi công thức tối đa **2000 ký tự**. Công thức sai, dùng cú pháp chưa hỗ trợ hoặc vượt giới hạn sẽ được bỏ qua kèm thông báo, không làm mất nội dung trả lời. Khi stream bị ngắt, bot vẫn preview các công thức đã hoàn chỉnh trong phần nhận được.

## Trang web và API

Web server dùng `PORT` (mặc định `3000`). Khi chạy tại máy cá nhân, mở `http://localhost:3000`; khi triển khai, dùng tên miền của dịch vụ đang chạy bot.

| Đường dẫn | Nội dung |
| --- | --- |
| `/` | Trạng thái bot, số máy chủ, uptime và thống kê tổng hợp. |
| `/health` | Health check trả `{ "status": "ok" }` khi web server đã mở. |
| `/api/status` | Trạng thái và thống kê dạng JSON. |
| `/suggest` | Giao diện tra từ có thể nối tiếp. |
| `/invite` | Chuyển đến Discord để thêm bot đang chạy vào máy chủ. |
| `/api/suggest?word=học%20sinh` | Danh sách từ nối tiếp dạng JSON; thiếu hoặc rỗng `word` trả HTTP 400. |
| `/privacy-policy` | Privacy Policy bằng tiếng Anh. |
| `/terms-of-service` | Terms of Service bằng tiếng Anh. |

Bản build nhúng hai tài liệu [`PRIVACY_POLICY.md`](PRIVACY_POLICY.md) và [`TERMS_OF_SERVICE.md`](TERMS_OF_SERVICE.md). Khi sửa tài liệu, build lại và chạy binary mới để cập nhật trang web. Sau khi triển khai, có thể dùng URL công khai tương ứng để cấu hình các liên kết chính sách cho ứng dụng Discord.

## Triển khai và failover

Build trên máy Linux hoặc trong môi trường có cùng ABI với máy chạy. Triển khai binary `target/release/orenjineko` cùng các thư viện hệ thống được liệt kê ở trên. Cấu hình biến môi trường và chuyển tiếp HTTP tới cổng `PORT`. Dùng `/health` để kiểm tra HTTP, và `/api/status` để kiểm tra trạng thái kết nối Discord.

Chỉ chạy **một instance hoạt động tại một thời điểm**. Mỗi instance giữ cache RAM riêng và không nhận cập nhật trực tiếp từ instance khác. Khi chuyển VPS, dừng instance cũ trước rồi khởi động instance mới; bot mới nạp lại state từ cùng Supabase. Việc sao lưu và khôi phục dữ liệu thực hiện trên cơ sở dữ liệu; không dựa vào thư mục `data/` cũ.

## Cấu trúc dự án và dữ liệu

| Đường dẫn | Vai trò |
| --- | --- |
| `src/main.rs` | Khởi động, điều phối thao tác theo máy chủ và xử lý tín hiệu dừng. |
| `src/gateway.rs` | Kết nối Discord Gateway, giữ phiên, cập nhật cache và định tuyến sự kiện. |
| `src/discord.rs` | Discord REST, phản hồi tương tác và tính quyền trong kênh. |
| `src/commands.rs` | 13 slash command, autocomplete và nút duyệt từ. |
| `src/game.rs` | Lệnh prefix và xử lý lượt nối từ/đếm số. |
| `src/store.rs` | Supabase PostgREST, cache trạng thái, thống kê và từ điển. |
| `src/ai.rs` | Ngữ cảnh AI, ảnh, SSE và hiển thị câu trả lời. |
| `src/latex.rs` | Phân tích và dựng preview PNG công thức. |
| `src/web.rs` | Web server HTTP, API trạng thái, tra từ và hai trang chính sách. |
| `supabase/schema.sql` | Schema tương thích Phobophake và ghi chú migration. |

| Bảng Supabase | Dữ liệu |
| --- | --- |
| `words` | Từ điển chính, seed từ GitHub nếu rỗng. |
| `report_words` | Danh sách đen dùng chung. |
| `guild_config` | Kênh nối từ và chế độ bot, khoá chính `channel_id`. |
| `game_state` | Trạng thái ván nối từ theo kênh. |
| `counting_channels` | Kênh đếm số, số hiện tại và người đếm gần nhất. |
| `rankings` | Xếp hạng theo máy chủ/người dùng; `true_count` lưu số từ đúng. |
| `global_stats` | Các bộ đếm `query`, `word_played`, `round_played`. |
| `premium_guilds` | ID máy chủ Premium. |

Bot nạp dữ liệu vào RAM khi khởi động và lưu các lượt chơi qua Supabase. Bộ đếm `query` gom trong RAM, ghi mỗi 60 giây và khi nhận `SIGINT` hoặc `SIGTERM`. Khi có lỗi ghi dữ liệu, cache chỉ cập nhật các hàng đã được Supabase xác nhận. Từ điển trò chơi lấy từ bảng `words` sau khi trừ các mục trong `report_words`.

## Kiểm tra mã nguồn

```bash
cargo fmt --check
cargo clippy --locked --offline --all-targets -- -D warnings
cargo test --locked --offline
```

Các bài kiểm thử chạy với dữ liệu nội bộ và không cần tài khoản Discord, Supabase hoặc AI.

## Nguồn ngữ liệu và ghi nhận

- Dự án gốc: [lvdat/bot-noi-tu](https://github.com/lvdat/bot-noi-tu).
- Từ điển chính: [undertheseanlp/dictionary](https://github.com/undertheseanlp/dictionary), lọc cụm 2 tiếng phù hợp với trò chơi.
- Từ đóng góp được dùng khi seed: [lvdat/phobo-contribute-words](https://github.com/lvdat/phobo-contribute-words).

Các nguồn seed thực tế được cấu hình trong [`src/store.rs`](src/store.rs). Repository hiện tại của OrenjiNeko là [hakolilim/orenjineko](https://github.com/hakolilim/orenjineko).
