<p align="center">
  <a href="https://getartcraft.com/">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="docs/brand/artcraft-logo-white.svg">
      <img alt="ArtCraft" src="docs/brand/artcraft-logo.svg" width="200">
    </picture>
  </a>
</p>


<h1 align="center">FilmCraft</h1>

<p align="center">
  <b>تحرير الفيديو واللون والصوت؛ إعادة تنفيذ مفتوحة المصدر لبرنامج Adobe Premiere Pro وفق منهجية الغرفة النظيفة، مبنية بلغة Rust خالصة.</b>
</p>

<p align="center">
  قراءتنا المفتوحة المصدر، وفق منهجية الغرفة النظيفة، لسير عمل Adobe Premiere Pro: أصلي على macOS وWindows وLinux، وفي المتصفح عبر WebAssembly.<br>
  من فريق ArtCraft.
</p>

<p align="center">
  <a href="#الترخيص-والإسنادات"><img alt="الترخيص: MIT أو Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-8b5cf6"></a>
  <img alt="مكتوب بلغة Rust خالصة" src="https://img.shields.io/badge/pure-Rust-6a3fd6?logo=rust&logoColor=white">
  <img alt="يعمل على macOS وWindows وLinux" src="https://img.shields.io/badge/runs%20on-macOS%20%7C%20Windows%20%7C%20Linux-8b5cf6">
  <a href="#الحالة"><img alt="الحالة: فتيّ ويتقدّم بسرعة" src="https://img.shields.io/badge/status-young%20and%20moving%20fast-6a3fd6"></a>
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="انضم إلى مجتمع ArtCraft على Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<p align="center">
  <a href="https://getartcraft.com/apps/filmcraft"><b>FilmCraft على getartcraft.com</b></a> ·
  <a href="https://getartcraft.com/">ArtCraft</a> ·
  <a href="https://getartcraft.com/apps">كل Crafting Apps</a>
</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.ar.md">العربية</a>
</p>

<div dir="rtl">

<br>

<p align="center">
  <img src="docs/images/filmcraft-hero.png" alt="‏FilmCraft في مساحة عمل اللون، في منتصف مونتاج فيلم وثائقي عن مهمة أبولو 11 من لقطات NASA: شاشة البرنامج تعرض صاروخ Saturn V وهو يتجاوز برج الإطلاق مع شريط عنوان سفلي لمجمع الإطلاق 39A وترجمة حوار جو-أرض، وضوابط الآثار عليها إطارات مفاتيح Lumetri وScale للقطة، ولوحة Lumetri Color، وحاويات لقطات NASA المنتقاة، وخط زمني فيه 49 قطعة صورة، ولقطات مساعدة، وعناوين، ومسار تسميات توضيحية، وصوت المهمة، وموسيقى خلفية مُنزَّلة الجهارة، وعلامات مسماة، ومقطع مصيَّر مسبقاً، وعدادات جهارة حية" width="100%">
</p>

<p align="center"><sub><i>أبولو 11 - السكينة</i>: مونتاج وثائقي مدته ثلاث دقائق لإطلاق NASA عام 1969 وهبوطها على القمر وسيرها عليه، مع ترجمات مأخوذة من محضر المحادثات الصوتية للمهمة. كل إطار في هذه اللقطات مصدره لقطات من الملك العام، فُكّ ترميزها ورُكِّبت ودُرِّجت ألوانها بشيفرة FilmCraft نفسها.</sub></p>

> [!NOTE]
> **ArtCraft مجتمع من الفنانين من كل المشارب.** فن رقمي، وفن توليدي، وموسيقى، وألعاب &mdash; إن كنت تصنع الأشياء فأنت منّا. **[تعالَ وسلّم علينا في Discord](https://discord.gg/artcraft).**

<p align="center">
  <a href="#التحرير">التحرير</a> ·
  <a href="#اللون">اللون</a> ·
  <a href="#الآثار-والحركة">الآثار</a> ·
  <a href="#الصوت">الصوت</a> ·
  <a href="#العناوين-والتسميات-التوضيحية">العناوين</a> ·
  <a href="#الصيغ-والترميز">الصيغ</a> ·
  <a href="#التصدير">التصدير</a> ·
  <a href="#التبادل">التبادل</a> ·
  <a href="#مبني-للوكلاء">الوكلاء</a> ·
  <a href="#البدء-السريع">البدء السريع</a> ·
  <a href="#التنزيلات">التنزيلات</a> ·
  <a href="#التوثيق">التوثيق</a> ·
  <a href="#الحالة">الحالة</a>
</p>

<br>

‏FilmCraft محرّر غير خطي لمن يعرفون Premiere: اللوحات ومساحات العمل والأدوات والاختصارات نفسها، فيداك تعرفان موضع كل شيء سلفاً. أما من الداخل فهو جديد من مجرى البتات صعوداً. مرمّزات H.264 وHEVC وProRes وAAC مرمّزاتنا نحن، كُتبت بلغة Rust من المواصفات العامة. ومركِّب يعمل على وحدة معالجة الرسوميات في الضوء الخطي. وحسابات الإطارات تجري على زمن صحيح من الأعداد الصحيحة، فلا تنزلق التحريرات أبداً. وكل إجراء في التطبيق أمرٌ يستطيع وكيل الذكاء الاصطناعي قيادته بالدقة نفسها التي تقودها بها أنت.

<br>

## التحرير

<p align="center">
  <img src="docs/images/filmcraft-assembly.png" alt="مساحة عمل التجميع: حاويات مصغرات الأفلام، وفيلم كرنفال الأرواح في شاشة المصدر، ولقطة مقبرة من فيلم ليلة الموتى الأحياء في شاشة البرنامج، والإعلان الدعائي على خط زمني ثلاثي المسارات مع موسيقى شوبرن على A2 وعدادات الجهارة" width="100%">
</p>

<p align="center"><sub>مساحة عمل التجميع، أثناء مونتاج إعلان دعائي لفيلم <i>ليلة الموتى الأحياء</i> ‏(1968) مع لقطة مُدرجة من <i>كرنفال الأرواح</i> ‏(1962).</sub></p>

**خط زمني تعرفه سلفاً.** شاشتا المصدر والبرنامج، وحاويات بمصغرات، وخط زمني متعدد المسارات بأزرار الترقيع والاستهداف، وأقفال المزامنة والتحديد المرتبط. مساحات عمل التحرير والتجميع واللون والتأثيرات والصوت كلها موجودة، وكل لوحة ترسو حيث تشاء.

- **تحرير بثلاث نقاط.** علّم نقطتَي الدخول والخروج في شاشة المصدر، ثم أدرج ‏(`,`) أو استبدل (`.`) على المسارات المرقّعة. ويرفع (`;`) ويستخرج (`'`) النطاقات خارجاً من جديد.
- **كل أنواع التقليم.** أدوات التقليم التتابعي والتحرير التدحرجي وتحريك المحتوى والانزلاق وتمدد المعدل والموسى. يحدد وضع التقليم نقاط التحرير كتقليم تتابعي أو تدحرجي أو تقليم، ويزحزحها إطاراً إطاراً (`⌥←` `⌥→`، وبمقدار خمسة أضعاف مع `⇧`)، ويبدّل نوع التقليم بـ `⌃T` ويمدّها إلى رأس التشغيل بـ `E`.‏ و`Q` و`W` يقلمان تتابعياً حتى رأس التشغيل. وتعرض **شاشة التقليم** جانبَي التحرير، و**التقليم الديناميكي** يقلم مباشرةً أثناء التشغيل: ‏`L` أماماً، و`J` خلفاً، و`K` للإيقاف والاعتماد خطوة تراجع واحدة.
- **زمن مضبوط.** كل تحرير يُحسب على نقرات صحيحة: 254,016,000,000 نبضة في الثانية، وهو عدد يقبل القسمة على كل معدلات الإطارات ومعدلات العينات الشائعة دون باقٍ. فمعدلات 23.976 و29.97 بإسقاط الإطارات و59.94 مضبوطة لا تقريبية.
- **التفاصيل التي يعتمد عليها المحترفون.** علامات بألوان وأسماء ومدد؛ تصدير ملاحظات مراجعة التسلسل عبر **علامات ▸ تصدير العلامات بصيغة CSV…**؛ إضافة تحرير (`⌘K`) على مسار واحد أو كل المسارات؛ التداخل الهرمي؛ النسخ واللصق واللصق والإدراج؛ الحذف التتابعي وإغلاق الفجوة؛ الالتقاط؛ تراجع غير محدود مع لوحة سجل الإجراءات.
- **مفاتيحك أنت.** محرّر اختصارات لوحة المفاتيح (`⌥⌘K`) بلوحة مفاتيح مرسومة، واختصارات خاصة بكل لوحة، وتحذيرات من التعارض، وإعدادات مسبقة لتخطيطات مفاتيح FilmCraft وPremiere Pro وFinal Cut Pro وAvid.
- **لا تفقد عملك أبداً.** الحفظ ذرّي، والحفظ التلقائي يحتفظ بمجموعة متجددة من الإصدارات، وسجل الاسترداد بعد الانهيار، الذي يُكتب بعد نحو ثانية من كل تحرير، يعيد التغييرات غير المحفوظة بعد انهيار أو انقطاع كهرباء.

<p align="center">
  <img src="docs/images/filmcraft-timeline.png" alt="تقريب على الخط الزمني: لقطات ليلة الموتى الأحياء مع تلاشيات متبادلة على V1، ولقطة مُدرجة من كرنفال الأرواح على V2، ونوكتيورن شوبرن موجةً صوتية خضراء على A2، وعلامات ملوّنة على طول المسطرة، وعداد الجهارة يقرأ القيم اللحظية والقصيرة والمتكاملة وذروة القمة الحقيقية" width="100%">
</p>

<p align="center"><sub>عن قرب: تلاشيات متبادلة، ولقطة مُدرجة على V2، والموسيقى التصويرية على A2، وعلامات ملوّنة، وعدادات الجهارة على اليمين.</sub></p>

<br>

## اللون

<p align="center">
  <img src="docs/images/filmcraft-color.png" alt="مساحة عمل اللون على فيلم شارد (1963): كاري غرانت وأودري هيبورن في شاشة البرنامج، وشكل الموجة والمتجهاتي على اليسار، وحاويات الإعلان الدعائي والخط الزمني أدناه" width="100%">
</p>

<p align="center"><sub>مساحة عمل اللون على فيلم <i>شاراد</i> ‏(1963)، مع مناظير حيّة بجوار شاشة البرنامج.</sub></p>

**لوحة تدريج ألوان كاملة على طراز Lumetri**، بالترتيب الذي يعمل به مدرّج الألوان:

- **التصحيح الأساسي:** الحرارة والصبغة، والتعريض، والتباين، وأبرز المناطق، والظلال، والأبيض، والأسود، والتشبع.
- **الإبداعي:** ثمانية مظاهر مدمجة مع تحكم بالشدة، إضافة إلى مظهر الفيلم الباهت وزيادة الحدة والحيوية. المظاهر تحويلات لونية مكتوبة في الشيفرة، لا ملفات LUT.
- **المنحنيات:** محرّر منحنيات RGB باستيفاء مكعبي أحادي النبرة (لا تجاوز)، وتدرج لوني مقابل تشبع، وتدرج لوني مقابل تدرج لوني، وتدرج لوني مقابل إضاءة، وإضاءة مقابل تشبع، وتشبع مقابل تشبع.
- **عجلات الألوان:** عجلات الظلال والدرجات الوسطى وأبرز المناطق، ولكل واحدة منها تحكم خاص بدرجة الإضاءة.
- **HSL ثانوي:** استخرج مفتاح لون حسب التدرج اللوني والتشبع والإضاءة، وعاين المفتاح قناعاً، وصحّح ما حددته أنت فقط.
- **تظليل الحواف:** المقدار، ونقطة المنتصف، والاستدارة، والتنعيم.

<table>
  <tr>
    <td width="34%" valign="top" align="center">
      <img src="docs/images/filmcraft-lumetri.png" alt="لوحة Lumetri Color على فيلم شارد (1963): منزلقات التصحيح الأساسي بمسارَي حرارة وصبغة ملوّنين، وقسما الإبداعي والمنحنيات مطويان، وعجلات ألوان الظلال والدرجات الوسطى وأبرز المناطق، ومبدّلا HSL ثانوي وتظليل الحواف"><br>
      <sub><b>لوحة Lumetri Color.</b> التصحيح الأساسي، وعجلات الألوان، وكل قسم آخر، ولكل منها مفتاح تشغيل/إيقاف خاص.</sub>
    </td>
    <td width="66%" valign="top" align="center">
      <img src="docs/images/filmcraft-scopes.png" alt="مناظير Lumetri: موجة إضاءة خضراء ومتجهاتي لإطار من فيلم شارد (1963)"><br>
      <sub><b>مناظير تقول الحقيقة.</b> يُحسب شكل الموجة والمتجهاتي من الإطار المدرَّج لوناً. وكل التدريج يجري في الضوء الخطي، وبفاصلة عائمة 32 بت.</sub>
    </td>
  </tr>
</table>

<br>

## الآثار والحركة

<p align="center">
  <img src="docs/images/filmcraft-effects.png" alt="مساحة عمل الآثار: ضوابط الآثار تعرض الحركة والعتامة وإعادة تعيين الوقت وLumetri Color، ومشاهدات الأرض من محطة الفضاء الدولية لوكالة NASA في شاشة البرنامج، ومستعرض الآثار مفتوح على انتقالات التلاشي المتبادل" width="100%">
</p>

<p align="center"><sub>مشاهدات الأرض من محطة الفضاء الدولية لوكالة NASA مع تطبيق مظهر، ومستعرض الآثار مفتوح على التلاشيات المتبادلة.</sub></p>

- **حاوية Video Effects الكاملة لبرنامج Premiere 26 ‏(93 تأثيراً في 16 مجلداً)، وحاوية Legacy، والآثار المتقادمة التي تستخدمها المشاريع القديمة، إضافة إلى نحو 30 انتقالاً.** ضبابيات ‏(بوكيه، وتمويه البؤرة، والتمويه المركب)، ومفاتيح ‏(المفتاح الفائق، ومالص المسار)، وتحريفات ‏(تثبيت الزوايا، والإزاحة المضطربة، ومثبّت الاعوجاج)، وإضاءات وتوهجات، وآثار الفيديو الغامر ‏(VR) على اللقطات متساوية المستطيلات، وتدرج زمني، وصدى وغيرها. تلاشٍ متبادل، وتلاشٍ إلى الأسود أو الأبيض، وذوبان فلمي، ومسوح، وقزحيات، ودفعات، وشرائح، وتكبيرات، وتقشير صفحة، ودوران مكعب وغيرها.
- **حركة وعتامة على كل مقطع:** الموضع، والتحجيم، والدوران، ونقطة الإرساء، ومقاومة الوميض، إضافة إلى 26 وضع دمج.
- **إطارات مفاتيح مثل إطارات Premiere:** خطي، وبيزييه، وتلقائي، وبيزييه متصل، وتثبيت، وتباطؤ للداخل وتباطؤ للخارج. تعرض ضوابط الآثار مسار إطارات مفاتيح لكل معامل تحت مسطرة زمنية عليها مقبض رأس التشغيل، وكل معامل متحرك ينفتح إلى **مخططات قيمة وسرعة** بمقابض تأثير قابلة للسحب. وتتقاسم ضوابط الآثار ولوحة الخصائص متصفح إطارات المفاتيح (◀ ◆ ▶): أضف إطار المفتاح عند رأس التشغيل أو أزله، وانتقل إلى السابق أو التالي.
- **مركِّب على وحدة معالجة الرسوميات** مبني على wgpu ‏(Metal وVulkan وDirectX 12 وWebGPU). يأخذ عينات YUV من فاك الترميز مباشرةً بترشيح فائق بدقة الأثر ‏(footprint supersampling) ويمزج في الضوء الخطي. ومسار على المعالج يصيّر الإطارات نفسها، ويُختبر أحدهما مقابل الآخر.

<p align="center">
  <img src="docs/images/filmcraft-keyframes.png" alt="ضوابط الآثار مع مخططي القيمة والسرعة لتكبير متباطئ على بطاقة عنوان فيلم ليلة الموتى الأحياء، وشاشة البرنامج تعرض العنوان فوق طريق ريفي، ولوحة الخصائص تعرض التحويل والقص" width="100%">
</p>

<p align="center"><sub>تكبير متباطئ نحو بطاقة العنوان: مخطط القيمة، ومخطط السرعة، ومقبض تأثير بيزييه.</sub></p>

<br>

## الصوت

- **عدادات جهارة وفق معايير البث.** الجهارة اللحظية والقصيرة المدى والمتكاملة وذروة القمة الحقيقية وفق ITU-R BS.1770 وEBU R128، إلى جانب عدادات القمة. قراءتنا المتكاملة تطابق مرشح `ebur128` في ffmpeg حتى عُشر وحدة LU.
- **آثار مقطع على مكتبة معالجة إشارات رقمية خاصة بنا:** معادل EQ بارامتري، ومرشحات عالية العبور ومنخفضة العبور وممرّة النطاق، وديناميكيات، ومحدد ذروة قمة حقيقية، وتأخير، وصدى، ومزيل التشويش ‏(DeNoise)، ومزيل الطنين ‏(DeHummer)، وعكس، وإزاحة درجة الصوت. يمكن وضع المعاملات في إطارات مفاتيح. وتبقى الآثار متصلة عبر التمرير والتشغيل والتصدير، مع تعويض زمن الاستجابة.
- **مازج مسارات الصوت ومازج مقاطع الصوت:** منزلقات، وإزاحة أحادية، وكتم، وإفراد، وخمس شقوق إدراج لكل مسار ‏(قبل المنزلق أو بعده)، ومخارج إرسال، ومزجات فرعية، ومسار مزج، وكل ذلك مضبوط بدقة العينة مع تعويض زمن الاستجابة. 24 مساراً بثلاثة آثار لكل منها تُخلط أسرع نحو ست مرات من الزمن الحقيقي على نواة واحدة.
- **أتمتة على طريقة أجهزة المزج:** أوضاع القراءة والمزلاج واللمس والكتابة تُسجَّل مباشرةً من المازج أثناء التشغيل، وتُعرض وتُحرَّر إطاراتِ مفاتيح على مسارات الخط الزمني. إضافة إلى مربع حوار معامل الصوت ‏(تعيين، وتعديل، وتسوية) وتلاشيات متقاطعة بقدرة ثابتة.
- **تشغيل بساعة الصوت.** بطاقة الصوت هي الساعة الرئيسة، فتتبع الصورةُ الصوتَ ولا يكون العكس أبداً.

<br>

## العناوين والتسميات التوضيحية

- **محرك نصوص حقيقي:** تشكيل OpenType مع تقريب الحروف ‏(kerning) والروابط، ونص ثنائي الاتجاه، وكسر الأسطر، والتباعد الحرفي ‏(tracking) والإراحة ‏(leading)، مرسوم في الضوء الخطي وحاد عند أي تحجيم أو دوران.
- **أدوات النص والشكل والقلم** على شاشة البرنامج: انقر للكتابة، وحرّر بمؤشر إدراج وتحديد، واسحب مستطيلات وقطوعاً ناقصة ومسارات. تضم المقاطع الرسومية طبقات نصوص وأشكال بتعبئة وحدود وخلفية وظل، كلها قابلة لإطارات المفاتيح، وتُحرَّر في لوحة الخصائص.
- **التسميات التوضيحية:** مسارات تسميات توضيحية بصيغ العنوان الفرعي وCEA-608 وCEA-708 والنص التلفازي؛ واستيراد وتصدير SRT وWebVTT وSCC بتوقيت مضبوط حتى الإطار؛ وتحرير التسميات في لوحة النصوص وحرقها في التصديرات.
<br>

## الصيغ والترميز

لا FFmpeg في الداخل. مرمّزات الفيديو وAAC وOpus والحاويات شيفرة Rust خاصتنا، منفَّذة من المواصفات العامة لـ ITU-T وISO وIETF ومختبرة إطاراً بإطار مقابل ffmpeg بوصفه حَكَماً خارجياً.

| | فك الترميز | الترميز | ملاحظات |
|---|:---:|:---:|---|
| **H.264 / AVC** | ✓ | ✓ | فاك الترميز مطابق بالبت على أكثر من 37 مجرى، 500–600 إطاراً في الثانية عند 1080p. المرمِّز: أنماط High وMain وBaseline، وCABAC، وإطارات B، وCRF/CBR/VBR/تمريران |
| **HEVC / H.265** | ✓ | | Main وMain 10، مطابق بالبت على 41 مجرى ‏(بلاطات، وWPP، وPCM، ومراجع طويلة الأمد)، نحو 225 إطاراً في الثانية عند 1080p |
| **Apple ProRes** | ✓ | ✓ | يفك الترميز كل شيء من 422 Proxy إلى 4444 XQ؛ والتصدير يكتب 422 HQ |
| **AAC-LC** | ✓ | ✓ | |
| **VP8** | ✓ | | كل أدوات الترميز، وإصدارات مجرى البت 0–3، وإطارات altref المخفية، وطبقة ألفا في WebM الشفاف؛ مطابق بالبت مع libvpx على متجهات اختبار VP8 المنشورة؛ في WebM/Matroska وMP4 |
| **VP9** | ✓ | | الأنماط 0–3، و8/10/12 بت، ومن 4:2:0 إلى 4:4:4، وبلاطات وإطارات فائقة؛ مطابق بالبت على أكثر من 50 مجرى؛ في WebM/Matroska وMP4 |
| **Opus** | ✓ | | SILK وCELT والهجين والمحيط متعدد المجاري؛ مطابق بالنطاق على كل متجه مطابقة في RFC 8251؛ في WebM/Matroska وMP4 |
| **AV1** | ✓ | | النمط Main، و8/10 بت 4:2:0 وأحادي اللون، وكل أدوات الترميز بما فيها حبيبات الفيلم والدقة الفائقة والطبقات المكانية؛ مطابق بالبت مع libdav1d على متجهات libaom المختبرة؛ في MP4 وWebM/Matroska |
| **Avid DNxHD / DNxHR** | ✓ | ✓ | كل معرفات SMPTE ST 2019-1 ‏(من LB إلى 444، و8/10/12 بت)؛ والتصدير يكتب DNxHR في MOV |
| **MJPEG, PCM** | ✓ | ✓ | |
| **MP3, FLAC, ALAC, Vorbis** | ✓ | | عبر صندوقة [symphonia](https://github.com/pdeljanov/Symphonia) ‏(MPL-2.0) مؤقتاً، وستُستبدل بتنفيذ خاص بنا |
| **MP4 / MOV** | ✓ | ✓ | MP4 مجزأ، وقوائم تحرير، ومسارات رمز زمني |
| **Matroska / WebM** | ✓ | | ربط ‏(lacing)، وإشارات ‏(Cues)، وتجريد الترويسات، وبيانات ألوان HDR وصفية |
| **AVI** | ✓ | | AVI 1.0 وOpenDML ‏(ملفات يتجاوز حجمها 1 GB)؛ وMotion JPEG وH.264 وHEVC وRGB / YUV غير مضغوط مع PCM أو MP3 أو MP2 أو AC-3. أما DivX / Xvid وDV فليس بعد |
| **الصور الثابتة** | ✓ | ✓ | استيراد PNG وJPEG وGIF وWebP وTIFF وBMP ‏(GIF وAPNG المتحركة مقاطع، بالشفافية)؛ وتصدير متتاليات PNG وGIF متحركة |

يُستورد أيضاً: فيديو MPEG-2 / MPEG-1، وAC-3، وMP2، وMXF ‏(OP1a / OP-Atom)، ومجاري النقل والبرامج MPEG ‏(AVCHD، والبث، وDVD)، وOgg ومتتاليات الصور. ليس بعد: تصدير AV1، وصور الكاميرا الخام ‏(RAW)، وE-AC-3. يعمل فك الترميز بالعتاد على macOS ‏(VideoToolbox) وWindows ‏(Media Foundation)، ولِـ H.264 وHEVC على Linux ‏(VA-API)؛ ويستطيع تصدير H.264 استخدام مرمّز NVIDIA على Windows وLinux ‏(بتفعيل اختياري)، ويمر تصدير H.265 ‏(HEVC Main، ‏8 بت) عبره ‏(NVENC على Windows، وVideoToolbox على macOS) ‏([#30](https://github.com/storytold/filmcraft/issues/30)).

<br>

## التصدير

<p align="center">
  <img src="docs/images/filmcraft-export.png" alt="وضع التصدير: وجهات لملف الوسائط وYouTube وVimeo وTikTok وInstagram وFTP على اليسار، وإطار من فيلم ليلة الموتى الأحياء في المعاينة، وإعدادات H.264 على اليمين (1440x1080 عند 23.976 إطاراً في الثانية، و48000 هرتز ستيريو، والتسلسل بأكمله)" width="100%">
</p>

<p align="center"><sub>وضع التصدير، مضبوط لكتابة الإعلان الدعائي بصيغة H.264 MP4.</sub></p>

- **H.264 MP4 مع AAC، بمرمّزاتنا الخاصة.** تصيير مدته 6 ثوان بدقة 960×540 يستغرق 1.3 ثانية ويُفك ترميزه في ffmpeg دون تحذير واحد، مع كشف الأخطاء الصارم مفعّلاً.
- **وأيضاً:** Apple ProRes 422 HQ وMotion JPEG في QuickTime، وMXF OP1a وOP-Atom على طريقة Avid ‏(DNxHR أو ProRes أو H.264 مع PCM ورمز زمني للبداية)، ومتتاليات PNG، وGIF متحركة، وWAV.
- **مهام خلفية** مع تقدم وإلغاء، فتواصل التحرير بينما يجري التصيير.
- **معاينات التصيير:** يعلّم شريط التصيير المقاطع أخضر أو أصفر أو أحمر؛ وتُخزَّن المعاينات المصيَّرة مؤقتاً بحسب المحتوى، فلا يبطل التحرير إلا ما يلامسه، ويعيد التراجع الأخضرَ إلى حاله.

<br>

## التبادل

انقل الخطوط الزمنية بين FilmCraft وكل محرر آخر:

- **XML لبرنامج Final Cut Pro 7 ‏(xmeml):** الصيغة التي يتبادلها Premiere وDaVinci Resolve. تحمل تسلسلات متداخلة، وانتقالات، ومولدات، والحركة، والعتامة، وإعادة تعيين الوقت، ومستويات الصوت، بإطارات مفاتيحها.
- **FCPXML 1.9–1.11:** العمود الفقري، والمقاطع المتصلة ممراتٍ، والانتقالات، وإعادة ضبط السرعة، والمقاطع المركبة.
- **OpenTimelineIO:** صيغة التبادل المفتوحة لصناعة السينما، تُذهب وتُعيد كل تفصيلة من FilmCraft عبر `metadata.filmcraft`.
- **CMX 3600 EDL:** أقدم صيغة لا تزال قيد الاستخدام، برمز زمني بإسقاط الإطارات، وتلاشيات، ومسوح، وتغييرات سرعة (`M2`)، وملف EDL واحد لكل مسار.
- **AAF ‏(بروتوكول التحرير):** لبرنامج Avid Media Composer وPro Tools. مسارات فيديو وصوت، وتلاشيات وهبوطات، ومستوى صوت المقطع بإطارات مفاتيح، وعلامات ورمز زمني للمصدر؛ والصوت مضمّن أو ملفات WAV / AIFF منفصلة، مقلمة بهوامش، مع تصيير آثار المقطع داخلها وفصلها إلى أحادي، إضافة إلى مزج فيديو اختياري. والاستيراد يقرأ AAF من جديد مستخرجاً الصوت المضمّن.
- **OMF 2.0:** تسليم ما بعد الإنتاج الصوتي: مسارات الصوت بتلاشيات متقاطعة وكسب، بدقة العينة، والصوت محفوظ داخل الملف أو بجانبه.

يدمج الاستيراد حاويات المستند ووسائطه وتسلسلاته في مشروعك خطوةً واحدة قابلة للتراجع، ويصل كل ملف وسائط يجده على القرص.

<br>

## مبني للوكلاء

كل بند قائمة وزر ومنزلق وسحب في FilmCraft هو **أمر** له معرّف ومعاملات منمطة وحالة تمكين. هناك أكثر من 650 أمر محرك ‏(`filmcraft-cli commands` يسردُها)، وبقية فهرس Premiere في الطريق. الواجهة وسطر الأوامر وقناة تحكم بصيغة JSON و**خادم MCP** كلها تستدعي الأوامر نفسها، فيستطيع Claude أو أي وكيل أن يقصّ ويقلم ويدرّج ويمزج ويصدّر تماماً كما يفعل الإنسان. ويمكن أيضاً قيادة الواجهة على مستوى الفأرة ولوحة المفاتيح: لكل ودجة معرّف أتمتة، ويستطيع الوكلاء النقر والسحب والكتابة والتقاط اللقطات.

```jsonc
// عبر قناة التحكم (أسطر JSON على TCP) أو كاستدعاءات أدوات MCP
{"method": "engine.execute", "params": {"command": "timeline.place",
  "params": {"item": 1, "track": "V1", "seconds": 0, "sourceIn": 284298240000000, "duration": 1270080000000}}}
{"method": "engine.execute", "params": {"command": "effects.setParam",
  "params": {"clip": 87, "effect": "lumetri", "param": "curve_luma", "value": [[0,0],[0.25,0.21],[0.75,0.82],[1,1]]}}}
{"method": "ui.screenshot", "params": {"path": "frame.png"}}
```

الإعلان الدعائي والتدريجات في هذه اللقطات بُنيت بهذه الطريقة تماماً، بوكيل يقود التطبيق قيد التشغيل.

<br>

## في كل مكان

- **أصلي** على macOS وWindows وLinux، مع شريط قوائم macOS أصلي.
- **الويب:** المحرك نفسه والواجهة نفسها يعملان في المتصفح عبر WebAssembly ‏(`apps/filmcraft-web`، انظر [docs/web.md](docs/web.md))؛ وكل إصدار يرفقه بصيغة `filmcraft-web-<version>.zip`.
- **واجهة قابلة للتبديل.** الواجهة صندوقة واحدة ‏(`ui-egui`) فوق المحرك، فيمكن لواجهة أمامية مختلفة أن تحل محلها دون المساس بمنطق التحرير.

<br>

## البدء السريع

```sh
cargo run --release -p filmcraft                           # تطبيق سطح المكتب، مع مشروع تجريبي
cargo run --release -p filmcraft -- --control 9876         # مع خادم التحكم بأسطر JSON
cargo run --release -p filmcraft-cli -- commands           # سرد كل أوامر المحرك
cargo run --release -p filmcraft-cli -- mcp                # خادم MCP (بلا واجهة)
```

النصوص اليابانية في الواجهة وفي العناوين تأتي من [craft-fonts](https://github.com/storytold/craft-fonts)، وهو مُدخل بناء اختياري ‏(تُضمّه إصدارات النشر دائماً؛ ومن دونه يستخدم FilmCraft خطوطه الخاصة وخطوط النظام):

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR="$PWD/../craft-fonts" cargo run --release -p filmcraft
```

بروتوكول التحكم موثق في [docs/control-protocol.md](docs/control-protocol.md). عالقت؟ أو تريد عرض ما صنعته؟ اسأل في [Discord](https://discord.gg/artcraft).

## التوثيق

| | |
|---|---|
| [CONTRIBUTING.md](CONTRIBUTING.md) · [docs/contributing.md](docs/contributing.md) | التهيئة، وبوابات الجودة، وأعراف الإيداعات، وكيفية إضافة الأوامر والآثار والمرمّزات واللوحات والأصول |
| [AGENTS.md](AGENTS.md) | القواعد التي يجب أن يتبعها كل مساهم: الأصول، والغرفة النظيفة، والتراخيص |
| [docs/architecture.md](docs/architecture.md) | الطبقات، ونموذج البيانات، والأساس الزمني، ونظام الأوامر، ومسار التصيير والتصدير |
| [docs/testing.md](docs/testing.md) | اختبارات الوحدة والخصائص وحَكَم ffmpeg، ومعايير الدقة، والقياسات المرجعية |
| [docs/agents.md](docs/agents.md) | قيادة FilmCraft عبر MCP وقناة التحكم؛ وكيف يطوره الوكلاء |
| [docs/control-protocol.md](docs/control-protocol.md) | مرجع قناة التحكم وMCP |
| [docs/project-files.md](docs/project-files.md) | صيغة `.fcproj`، وترحيلات المخطط، والحفظ التلقائي والاسترداد بعد الانهيار |
| [docs/graphics.md](docs/graphics.md) · [docs/captions.md](docs/captions.md) | محرك النصوص، والمقاطع الرسومية والأدوات؛ ومسارات التسميات التوضيحية وصيغها |
| [ROADMAP.md](ROADMAP.md) | المرحلة، وأرقام المطابقة، واللغات، وما التالي، وسجل التقدم |
| [docs/target-app-parity.md](docs/target-app-parity.md) · [docs/gaps.md](docs/gaps.md) · [docs/roadmap.md](docs/roadmap.md) | تقييم المطابقة مقابل Premiere Pro، وقائمة الفجوات مرتبةً، والمعالم والتركيز الحالي |
| [docs/codec-parity.md](docs/codec-parity.md) · [docs/file-format-parity.md](docs/file-format-parity.md) · [docs/hardware-parity.md](docs/hardware-parity.md) · [docs/ui-parity.md](docs/ui-parity.md) · [docs/localization-parity.md](docs/localization-parity.md) | قوائم تحقق المطابقة بُعداً بُعداً |

## الحالة

‏FilmCraft فتيّ ويتقدّم بسرعة. التحرير والتقليم وتعدد الكاميرات والألوان وإطارات المفاتيح والآثار والعناوين والتسميات التوضيحية والمزج والمرمّزات والتصدير تعمل اليوم.

‏FilmCraft في مرحلة **ألفا**. نتابع رقمين ‏([docs/target-app-parity.md](docs/target-app-parity.md)، أُعيد قياسهما في 2026-10-10 مقابل Premiere Pro 26.5.2):

- **اتساع المزايا: نحو 86%.** بنود قوائم Premiere Pro وآثاره وانتقالاته ولوحاته وصيغه الموجودة في FilmCraft ‏(القوائم 92% والآثار 100% مقيسة).
- **الجهوزية للعمل الحقيقي: نحو 56%.** تقديرنا الصادق لقرب FilmCraft من أن يحل محل Premiere في مشاريع حقيقية.

أكبر الفجوات اليوم:

- **السرعة على اللقطات الضخمة.** يعمل فك الترميز بالعتاد على macOS وWindows، ولِـ H.264 وHEVC على Linux ‏(VA-API، أو NVDEC مع تعريف NVIDIA؛ أما VP9 وAV1 فيُفك ترميزهما برمجياً هناك). أوضاع الدمج وأكثر الآثار شيوعاً تعمل على وحدة معالجة الرسوميات، غير أن Lumetri والمفاتيح وتصيير التصدير لا تزال على المعالج؛ ويستطيع ترميز H.264 استخدام مرمّز العتاد على macOS وعلى Windows وLinux مع معالج رسوميات من NVIDIA ‏(بتفعيل اختياري، تصدير ▸ ترميز العتاد)، أما تصدير H.265 فبالعتاد فقط على كليهما ‏(تُصدَّر التسلسلات HDR بنمط Main 10 PQ / HLG عبر NVENC؛ وVideoToolbox يكتب 8 بت SDR) ‏([#30](https://github.com/storytold/filmcraft/issues/30)).
- **مشاريع Premiere.** لا يمكن فتح ملفات `.prproj` بعد؛ أحضر التسلسلات بصيغة FCP7 XML أو AAF أو OTIO.
- **لا مكونات إضافية.** لا استضافة VST3 / Audio Units أو OpenFX.
- **مرمّزات التسليم.** H.264 هو تصديرنا الوحيد بمرمّز تسليم برمجي؛ أما H.265 فيُصدَّر عبر مرمّز عتادي فقط ‏(NVIDIA على Windows: ‏Main بـ 8 بت SDR وMain 10 بـ HDR PQ / HLG؛ وVideoToolbox على macOS: ‏Main بـ 8 بت، SDR)؛ ولا تصدير AV1 بعد.
- **الوسائط الواقعية والمنصات.** فاكات الترميز لدينا مطابقة بالبت على مجاري المطابقة، لكن ملفات الكاميرات والهواتف في الواقع أقل اختباراً. وWindows وLinux يحظيان باختبار أقل بكثير من macOS.
- **مزايا الذكاء الاصطناعي.** قليلة حتى الآن؛ وتحويل الكلام إلى نص اختياري ومعطّل افتراضياً.

تقارير الأخطاء بلقطات حقيقية أنفع ما يمكنك إرساله إلينا: [افتح مشكلة](https://github.com/storytold/filmcraft/issues) أو أخبرنا في [Discord](https://discord.gg/artcraft).

## البنية

فضاء عمل Cargo طبقي:

| الطبقة | الصندوقات |
|---|---|
| المرمّزات والحاويات | `h264`, `h264enc`, `hevc`, `vp9`, `prores`, `aac`, `opus`, `isobmff`, `matroska`, `avi`, `bitstream` |
| الأساسات | `time`, `geom`, `color`, `frame`, `media`, `text`, `project`, `audio-dsp` |
| التحرير والتبادل | `edit`, `codecs`, `captions`, `format` (ملفات المشروع), `interchange` |
| التصيير والإخراج | `render`, `gpu`, `export` |
| المحرك | `engine`: سجل الأوامر، والجلسة، والتراجع، والمهام |
| الواجهات الأمامية | `ui-egui`, `automation` (MCP), `apps/filmcraft`, `apps/filmcraft-cli` |
| دعم الاختبار | `testkit` (حُكّام ffmpeg، وعُدد الاختبار), `golden` (اختبارات الصور الذهبية) |

لا شيء تحت الواجهات الأمامية يعتمد على عدة واجهة أو واجهة برمجية لنظام تشغيل. ويفحص `cargo xtask ci` التنسيق والتنبيهات والاختبارات وقواعد الطبقية وإسناد الأصول وبناء wasm.

## التنزيلات

**جديد على FilmCraft؟** نزّله من [صفحة FilmCraft على getartcraft.com](https://getartcraft.com/apps/filmcraft). فهذه أسهل طريقة لتثبيته.

**تبحث عن بنية أو صيغة بعينها؟** على GitHub يضم [أحدث إصدار](https://github.com/storytold/filmcraft/releases/latest) كل البُنى المذكورة أدناه، وتضم [كل الإصدارات](https://github.com/storytold/filmcraft/releases) الإصدارات السابقة وملاحظاتها. `<ver>` في أسماء الملفات هو رقم الإصدار، ويسرد `SHA256SUMS.txt` بصمة تحقق لكل ملف.

### Windows

| البنية | المُثبِّت | النسخة المحمولة |
|---|---|---|
| x64 ‏(Intel/AMD بمعمارية 64 بت) | `filmcraft-<ver>-windows-x64.msi` | `filmcraft-<ver>-windows-x64-portable.zip` |
| arm64 ‏(Snapdragon وحواسيب ARM الأخرى) | `filmcraft-<ver>-windows-arm64.msi` | `filmcraft-<ver>-windows-arm64-portable.zip` |
| x86 ‏(32 بت) | `filmcraft-<ver>-windows-x86.msi` | `filmcraft-<ver>-windows-x86-portable.zip` |

المثبّتات والملفات التنفيذية موقَّعة رقمياً.

**إذا لم يفتح التطبيق على Windows:** يهيّئ تطبيق سطح المكتب Vulkan وDirectX 12 فقط افتراضياً، لا OpenGL. وتمكين wgpu من إنشاء نسخة OpenGL أيضاً قد يُسقط بعض تعريفات الرسوميات ‏(`atio6axx.dll` من AMD) قبل ظهور النافذة، فيومض التطبيق في إدارة المهام ثم يخرج. يتجاوز `WGPU_BACKEND` الافتراضي لأغراض التشخيص ‏(مثلاً `dx12` أو `vulkan`). في PowerShell، من المجلد الحاوي الملف التنفيذي:

```powershell
$env:WGPU_BACKEND = "vulkan"
& .\filmcraft.exe
Remove-Item Env:WGPU_BACKEND                     # استعادة الافتراضي للتشغيلات اللاحقة
```

قد يعيد تجاوز `gl` الصريح انهيار التعريف على الأنظمة المتأثرة. أما افتراضيات macOS وLinux والويب فلم تتغير.

### macOS

| البنية | الملف | ملاحظات |
|---|---|---|
| التطبيق، شامل ‏(Apple silicon + Intel) | `filmcraft-<ver>-macos-universal.dmg` | موقَّع وموثَّق لدى Apple |
| أداة سطر الأوامر، شاملة | `filmcraft-cli-<ver>-macos-universal.zip` | موقَّعة وموثَّقة لدى Apple |

### Linux

| الصيغة | x86_64 | aarch64 ‏(ARM64) | ملاحظات |
|---|---|---|---|
| AppImage | `filmcraft-<ver>-linux-x86_64.AppImage` | `filmcraft-<ver>-linux-aarch64.AppImage` | يعمل في أي مكان؛ ويحدّث نفسه عبر [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) ‏(ملفات `.zsync`) |
| Flatpak | `filmcraft-<ver>-linux-x86_64.flatpak` | `filmcraft-<ver>-linux-aarch64.flatpak` | بيئة معزولة؛ `flatpak install --user <file>` |
| Debian/Ubuntu | `filmcraft-<ver>-linux-x86_64.deb` | `filmcraft-<ver>-linux-aarch64.deb` | |
| Fedora/RHEL/openSUSE | `filmcraft-<ver>-linux-x86_64.rpm` | `filmcraft-<ver>-linux-aarch64.rpm` | |
| حزمة tar | `filmcraft-<ver>-linux-x86_64.tar.gz` | `filmcraft-<ver>-linux-aarch64.tar.gz` | فك الضغط وشغّل `./install.sh` |

‏RISC-V ‏(riscv64): ‏`filmcraft-<ver>-linux-riscv64.tar.gz` ‏(حزمة tar فقط؛ وتحتاج glibc 2.39+، مثل Ubuntu 24.04 أو Debian 13).

يضع مثبّت حزمة tar كلاً من FilmCraft وأداة سطر أوامره وتكامل سطح المكتب في `~/.local`، دون صلاحيات مدير. ولكل المستخدمين شغّل `sudo ./install.sh --prefix /usr/local` بدلاً من ذلك. وإعادة تشغيل المثبّت تحدّث التثبيت.

**Gentoo ‏(يصونه المجتمع):** يحزم [طبقة ::snakebyte](https://github.com/switch87/snakebyte-overlay) إصدار Linux بصيغة `media-video/filmcraft-bin`. وهي من صيانة المجتمع لا من فريق FilmCraft، فأبلغ عن مشكلات التحزيم إلى الطبقة:

```sh
eselect repository add snakebyte git https://github.com/switch87/snakebyte-overlay.git
emaint sync -r snakebyte
echo 'media-video/filmcraft-bin ~amd64' >> /etc/portage/package.accept_keywords/filmcraft
emerge --ask media-video/filmcraft-bin
```

### FreeBSD

| البنية | الملف |
|---|---|
| x86_64 | `filmcraft-<ver>-freebsd-x86_64.tar.gz` |

### الويب (WebAssembly)

| البنية | الملف | ملاحظات |
|---|---|---|
| موقع ثابت | `filmcraft-web-<ver>.zip` | يعمل في متصفح حديث؛ استضفه على أي خادم ثابت |

## Crafting Apps

‏FilmCraft واحد من **Crafting Apps**: أدوات إبداعية مجانية ومفتوحة المصدر من فريق
[ArtCraft](https://getartcraft.com/)، كل واحدة منها مكتوبة بلغة Rust من الصفر، وكل واحدة قادرة على
الوقوف وحدها.

| | التطبيق | الغرض | الشيفرة | اعرف المزيد |
|:-:|---|---|---|---|
| <img src="https://raw.githubusercontent.com/storytold/photocraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.photocraft.png" alt="" width="32" height="32"> | **PhotoCraft** | معالجة الصور: طبقات وأقنعة ونصوص وملفات PSD حقيقية | [GitHub](https://github.com/storytold/photocraft) | [الموقع](https://getartcraft.com/apps/photocraft) |
| <img src="https://raw.githubusercontent.com/storytold/vectorcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.vectorcraft.png" alt="" width="32" height="32"> | **VectorCraft** | الرسم المتجه | [GitHub](https://github.com/storytold/vectorcraft) | [الموقع](https://getartcraft.com/apps/vectorcraft) |
| <img src="https://raw.githubusercontent.com/storytold/filmcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.filmcraft.png" alt="" width="32" height="32"> | **FilmCraft** | **تحرير الفيديو واللون والصوت · أنت هنا** | [GitHub](https://github.com/storytold/filmcraft) | [الموقع](https://getartcraft.com/apps/filmcraft) |
| <img src="https://raw.githubusercontent.com/storytold/lightcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.lightcraft.png" alt="" width="32" height="32"> | **LightCraft** | مكتبة الصور وتظهير ملفات raw | [GitHub](https://github.com/storytold/lightcraft) | [الموقع](https://getartcraft.com/apps/lightcraft) |
| <img src="https://raw.githubusercontent.com/storytold/pdfcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.pdfcraft.png" alt="" width="32" height="32"> | **PdfCraft** | قراءة ملفات PDF وتنظيمها وحمايتها | [GitHub](https://github.com/storytold/pdfcraft) | [الموقع](https://getartcraft.com/apps/pdfcraft) |
| <img src="https://raw.githubusercontent.com/storytold/effectcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.effectcraft.png" alt="" width="32" height="32"> | **EffectCraft** | الرسوم المتحركة والتأثيرات البصرية | [GitHub](https://github.com/storytold/effectcraft) | [الموقع](https://getartcraft.com/apps/effectcraft) |
| <img src="https://raw.githubusercontent.com/storytold/designcraft/main/assets/app-icon/hicolor/64x64/apps/ai.storyteller.designcraft.png" alt="" width="32" height="32"> | **DesignCraft** | تخطيط الصفحات والنشر | [GitHub](https://github.com/storytold/designcraft) | [الموقع](https://getartcraft.com/apps/designcraft) |

وكذلك [**ArtCraft**](https://getartcraft.com/) نفسه، استوديو الصور والفيديو بالذكاء الاصطناعي لدينا، للفنانين الذين يريدون تحكماً حقيقياً.

<br>

<p align="center">
  <a href="https://discord.gg/artcraft"><img alt="انضم إلى مجتمع ArtCraft على Discord" src="https://img.shields.io/badge/Join%20us%20on%20Discord-5865F2?style=for-the-badge&logo=discord&logoColor=white" height="40"></a>
</p>

<h3 align="center">تعال اصنع الأشياء معنا</h3>

<p align="center">
  Discord عندنا هو ملتقى صنّاع الفنون على اختلافهم: من يرسمون، ويصوّرون، وينحتون، ويحرّرون الأفلام،
  وينضدون الحروف، ومن ما زالوا يكتشفون ما يحبون صنعه. شاركنا ما تعمل عليه، واطلب العون،
  وأخبرنا بما تعطّل، أو بما تتمنى أن تفعله هذه الأدوات.
  مهما كان وسيطك ومهما طالت تجربتك، فمكانك بيننا.
</p>

<p align="center">
  <a href="https://discord.gg/artcraft"><b>discord.gg/artcraft</b></a> ·
  <a href="https://getartcraft.com/">getartcraft.com</a> ·
  <a href="https://getartcraft.com/apps">Crafting Apps</a> ·
  <a href="https://getartcraft.com/apps/filmcraft">FilmCraft</a>
</p>

<br>

## الترخيص والإسنادات

يُرخَّص FilmCraft ترخيصاً مزدوجاً بموجب [MIT](LICENSE-MIT) أو [Apache-2.0](LICENSE-APACHE)، أيهما تختار.
حقوق النشر (c) 2026 محفوظة لفريق ArtCraft ومساهمي FilmCraft. الإشعارات المطلوبة في [NOTICE](NOTICE).

تحتفظ الخطوط والأيقونات والصور وسائر الأصول المضمّنة بتراخيصها المفتوحة الخاصة بها؛ وكل واحد منها مدرج
باسم مؤلفه ومصدره وترخيصه في [ATTRIBUTION.md](ATTRIBUTION.md). أما البُنى المُنتَجة بـ
[craft-fonts](https://github.com/storytold/craft-fonts) ‏(كل الإصدارات الرسمية) فتضمّن خطوطه أيضاً
‏(OFL-1.1)، المدرجة في [ATTRIBUTION.md](https://github.com/storytold/craft-fonts/blob/main/ATTRIBUTION.md) الخاص به.

**اللقطات والموسيقى في الصور:** أفلام وتسجيلات التلفاز لمهمة أبولو 11 من وكالة NASA عبر
[images.nasa.gov](https://images.nasa.gov) ‏(الإطلاق، ومركز ضبط الإطلاق، وسطح القمر، والاستعادة)،
ومحضر المحادثات الصوتية جو-أرض لأبولو 11، وكلها أعمال حكومية أمريكية في الملك العام
‏(لا توصي NASA بهذا المشروع)؛ و<i>ليلة الموتى الأحياء</i> ‏(1968)، و<i>كرنفال الأرواح</i> ‏(1962)،
و<i>شاراد</i> ‏(1963)، وكلها في الملك العام الأمريكي؛ و<i>مشاهدات الأرض من محطة الفضاء الدولية</i> لوكالة
NASA؛ ونوكتيورن شوبرن المصنف 48 رقم 1 وبالاد رقم 1، بأداء من أجل Musopen ومنشورة برخصة CC0.
الوسائط نفسها ليست في هذا المستودع. مصادر كل أصل وتفاصيله في [ATTRIBUTION.md](ATTRIBUTION.md).

‏FilmCraft تنفيذ مستقل. لا يحتوي أي شيفرة من Adobe ولا أيقوناتها أو صورها أو إعداداتها المسبقة أو
ملفات LUT، ولا أي شيفرة GPL أو LGPL؛ كل أيقونة مرسومة في الشيفرة وكل أصل مرخّص ترخيصاً مفتوحاً
ومُسند ‏([AGENTS.md](AGENTS.md))، عدا اسم ArtCraft وشعاراته، وهي علامات تجارية لفريق ArtCraft
تُستخدم بموجب [`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt). ولا يُستخدم ffmpeg إلا حَكَمَ اختبارٍ خارجياً.

إن اسم ArtCraft وشعاره الكتابي وشعاراته في [`docs/brand/`](docs/brand/) علامات تجارية لفريق
ArtCraft وهي خارج نطاق هذا الترخيص. ولا يجوز استخدامها إلا دون تعديل، وفقط كجزء
من هذا المستودع ومن FilmCraft، بموجب [`docs/brand/LICENSE-brand.txt`](docs/brand/LICENSE-brand.txt).
وعلى النسخ المشتقة والنسخ المعدّلة إزالتها.

<sub>إن Adobe وPhotoshop وIllustrator وPremiere Pro وLightroom وAcrobat وAfter Effects وInDesign علامات تجارية أو علامات تجارية مسجلة لشركة Adobe Inc. في الولايات المتحدة و/أو بلدان أخرى. وFilmCraft مشروع مستقل مفتوح المصدر لا يتبع Adobe Inc. ولا تتكفّل به ولا تدعمه؛ ولا تُستخدم هذه الأسماء إلا لوصف سير العمل المتوافق معه.</sub>

<br>

<p align="center">
  <a href="https://getartcraft.com/"><img alt="ArtCraft" src="docs/brand/artcraft-mark.svg" width="28"></a><br>
  <sub>من إنتاج فريق <a href="https://getartcraft.com/">ArtCraft</a> ومجتمعه.</sub>
</p>

## تاريخ النجوم

[![مخطط تاريخ النجوم](https://api.star-history.com/svg?repos=storytold/filmcraft&type=Date&legend=top-left)](https://www.star-history.com/?repos=storytold%2Ffilmcraft&type=date&legend=top-left)

</div>
