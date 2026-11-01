#!/usr/bin/env python3
"""Subset an OFL Noto Sans CJK SC OTF for the bundled UI translations.
Requires fonttools and brotli. Usage: scripts/subset-ui-font.py SOURCE.otf
"""
import hashlib,json,pathlib,sys
from fontTools import subset
from fontTools.ttLib import TTFont
root=pathlib.Path(__file__).resolve().parent.parent
source=pathlib.Path(sys.argv[1])
text=''.join(json.loads((root/'web/locales/zh-CN.json').read_text()).values())+'简体中文语言文'
font=TTFont(source)
options=subset.Options();options.flavor='woff2';options.recalc_timestamp=False
subsetter=subset.Subsetter(options=options)
subsetter.populate(unicodes=sorted({ord(char)for char in text if ord(char)>=0x3000}))
subsetter.subset(font)
for platform,encoding,language in [(3,1,0x409),(1,0,0)]:
    for identifier,value in [(1,'Furbo UI SC'),(4,'Furbo UI SC Regular'),(6,'FurboUISc-Regular'),(16,'Furbo UI SC'),(17,'Regular')]:
        font['name'].setName(value,identifier,platform,encoding,language)
font.flavor='woff2'
output=root/'web/fonts/furbo-ui-sc.woff2';font.save(output)
(root/'web/fonts/SOURCE.json').write_text(json.dumps({
    'source':'https://github.com/notofonts/noto-cjk',
    'source_file':'Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf',
    'source_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
    'license':'OFL-1.1','modified_name':'Furbo UI SC','subset':'UI translation glyphs only'
},indent=2)+'\n')
print(f'Bundled Chinese glyph subset: {output.stat().st_size} bytes')
