#!/bin/sh
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Downloads source fonts, their licenses, and Adobe's Symbol/ZapfDingbats
# AFM files with their redistribution notice into sources/.
# Font binaries and AFM files are pinned and checked with SHA-256 below.
set -eu

DIR="$(dirname "$0")/../sources"
mkdir -p "$DIR"

ARIMO=https://raw.githubusercontent.com/googlefonts/arimo/4a6255f269916ae7ad3fc2706b0935e7621396b8
for style in Regular Bold; do
  curl -sSfL -o "$DIR/Arimo-$style.ttf" "$ARIMO/fonts/ttf/Arimo-$style.ttf"
done
curl -sSfL -o "$DIR/LICENSE_ARIMO" "$ARIMO/OFL.txt"
TINOS=https://raw.githubusercontent.com/googlefonts/tinos/3b4482a99b80ea5fc75f187b1be3120a3f5905b3
for style in Regular Bold Italic BoldItalic; do
  curl -sSfL -o "$DIR/Tinos-$style.ttf" "$TINOS/fonts/ttf/Tinos-$style.ttf"
done
curl -sSfL -o "$DIR/LICENSE_TINOS" "$TINOS/OFL.txt"

NOTO=https://raw.githubusercontent.com/notofonts/notofonts.github.io/781896b8653bf9e09129c103643a3cc5e31a2cb7/fonts
for font in NotoSansMono-Regular NotoSansMath-Regular NotoSansSymbols-Regular \
  NotoSansSymbols2-Regular; do
  family="${font%-*}"
  curl -sSfL -o "$DIR/$font.ttf" "$NOTO/$family/unhinted/ttf/$font.ttf"
done
curl -sSfL -o "$DIR/OFL.txt" \
  "https://raw.githubusercontent.com/notofonts/latin-greek-cyrillic/main/OFL.txt"

# The Core 14 AFM files of Adobe, as distributed by PDFBox.
AFM=https://raw.githubusercontent.com/apache/pdfbox/78be2c012e6b5f0c0b368dc9863fc01e9ec78f85/pdfbox/src/main/resources/org/apache/pdfbox/resources/afm
for file in Symbol.afm ZapfDingbats.afm MustRead.html; do
  curl -sSfL -o "$DIR/$file" "$AFM/$file"
done

LIBERATION=liberation-fonts-ttf-2.1.5
curl -sSfL -o "$DIR/$LIBERATION.tar.gz" \
  "https://github.com/liberationfonts/liberation-fonts/files/7261482/$LIBERATION.tar.gz"

cd "$DIR"
echo "7191c669bf38899f73a2094ed00f7b800553364f90e2637010a69c0e268f25d0  $LIBERATION.tar.gz" |
  sha256sum -c
for style in Regular Bold; do
  tar -xzf "$LIBERATION.tar.gz" --strip-components=1 \
    "$LIBERATION/LiberationMono-$style.ttf"
done
tar -xzOf "$LIBERATION.tar.gz" "$LIBERATION/LICENSE" > LICENSE_LIBERATION
rm "$LIBERATION.tar.gz"

sha256sum -c <<EOF
d7a8b187cf8444d4cfee102e8eae9e3043682fd5106d5d33ed677fe268a0e2ba  Arimo-Bold.ttf
41b22bc8f0b51f932825d37bc55b5eb6ba67dfe599a626e4aff2b43b624f9f8c  Arimo-Regular.ttf
c46fda6859efb80c0a3c1faa868908a3d2fcb26eb7fd9602d73bebeec21bca3a  LICENSE_ARIMO
cb3382d4643e8b02c12e322c220a3c76a5020d667e4fd4e7c75e744cca6caa6b  LICENSE_TINOS
bd62a0672d0b9b6710b01df434c80ad54fa5f0835207eb7b17b7a761463067bb  LiberationMono-Bold.ttf
f2b83c763e8afd21709333370bed4774337fae82267937e2b5aea7e2fbd922c1  LiberationMono-Regular.ttf
b226bfc00e1b8b8a80c7b3cfbbc322d13b4b0401f94cdeafdd93b2210ad802eb  MustRead.html
b127e84699212b6b2ef50aff58e0ebebeec04ffe6db1b9eb9e209c8c3d97b4aa  NotoSansMath-Regular.ttf
87f8ce0522a6c99b743ee5fc75b4073cfdd575639119672828b7b9944b65b4f4  NotoSansMono-Regular.ttf
6eea9cb4cd39269ea9f95ba5c2735f80ae74049dfc9e1a7c932a5cfc8f0c3030  NotoSansSymbols-Regular.ttf
c4a0a80f0041ce4be81e2478faad22776d23edb98ae3f0d19bd37044820ecf9d  NotoSansSymbols2-Regular.ttf
13eeef80def2f9a8d3a31a980731376823e8b48340ff3891ffc1cc0299c0b4fe  Symbol.afm
393269dbab8899f938db19783eca5eac92eb431f7ae0ab45b8349ca895f1a06b  Tinos-Bold.ttf
a5de79f0fe863ea0954757acb3d47b3ccd0a930ce3dd5b97230cd3866790a06e  Tinos-BoldItalic.ttf
5942266ed398b155d7dc23e36833e7ec6be988f2439bdbeb8ef1bede808eaa91  Tinos-Italic.ttf
60a0e8ef0c04dd5dd69ffe91025fa2ae5836cbd35600a82ba031977557e2cb61  Tinos-Regular.ttf
d3e7b9f6366ab0df06acbb217c8586c5962ff2efbd4e1b5566c7eaf5b745dd41  ZapfDingbats.afm
EOF
