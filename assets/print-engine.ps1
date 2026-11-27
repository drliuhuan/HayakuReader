param([string]$Mode, [string]$Job, [string]$Settings, [string]$OutputDirectory, [int]$PageStart = 0, [int]$PageCount = 5)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Drawing, System.Web.Extensions
$engine = @'
using System;
using System.IO;
using System.Linq;
using System.Text;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Printing;
using System.Web.Script.Serialization;

public class PrintMark { public int start,end; public string text,color,kind; public float[][] rects; }
public class SourcePage { public bool lazy; public string image; public float width,height; public PrintMark[] marks; }
public class PrintSource { public bool reflow; public string title; public float font_size; public SourcePage[] pages; }
public class ReaderPrintSettings {
 public string printer,paper,range,output; public bool landscape,color,highlights,notes,first_note_left;
 public int copies,duplex; public float margin_mm;
}
public class LocatedMark { public PrintMark mark; public List<RectangleF> rects=new List<RectangleF>(); }
public class LogicalPage : IDisposable {
 public int source_index; public int width,height; public RectangleF image_rect; public bool owns_file; public Bitmap bitmap; public string file; public List<LocatedMark> marks=new List<LocatedMark>();
 public void Dispose(){if(bitmap!=null)bitmap.Dispose();if(owns_file&&file!=null&&File.Exists(file))File.Delete(file);}
 public LocatedMark Locate(PrintMark mark){var found=marks.Find(m=>Object.ReferenceEquals(m.mark,mark));if(found==null){found=new LocatedMark{mark=mark};marks.Add(found);}return found;}
}
public class NoteItem { public LocatedMark mark; public List<string> lines; public float Height; }
public class Sheet { public LogicalPage page; public List<NoteItem> notes=new List<NoteItem>(); }
public static class ReaderPrintEngine {
 static JavaScriptSerializer Json(){return new JavaScriptSerializer{MaxJsonLength=Int32.MaxValue,RecursionLimit=100};}
 static T Read<T>(string path){return Json().Deserialize<T>(File.ReadAllText(path,Encoding.UTF8));}
 static void Write(string path,object value){File.WriteAllText(path,Json().Serialize(value),new UTF8Encoding(false));}
 static StringFormat Format(){var f=(StringFormat)StringFormat.GenericTypographic.Clone();f.FormatFlags|=StringFormatFlags.MeasureTrailingSpaces;return f;}
 static float Width(Graphics g,string text,Font font){if(text.Length==0)return 0;using(var f=Format())return g.MeasureString(text,font,100000,f).Width;}
 static int Fit(Graphics g,string text,int offset,Font font,float width){
  int newline=text.IndexOf('\n',offset);int end=newline<0?text.Length:newline;int low=0,high=end-offset;
  while(low<high){int n=(low+high+1)/2;if(Width(g,text.Substring(offset,n),font)<=width)low=n;else high=n-1;}
  if(low==0&&end>offset)low=1;
  if(low>0&&offset+low<text.Length&&Char.IsHighSurrogate(text[offset+low-1]))low--;
  if(low==0&&end>offset)low=Math.Min(2,end-offset);
  return low;
 }
 static List<string> Wrap(Graphics g,string text,Font font,float width){
  var lines=new List<string>();int offset=0;
  while(offset<text.Length){int n=Fit(g,text,offset,font,width);lines.Add(text.Substring(offset,n).TrimEnd('\r'));offset+=n;if(offset<text.Length&&text[offset]=='\n')offset++;}
  if(lines.Count==0)lines.Add("");return lines;
 }
 static void Quality(Graphics g){g.SmoothingMode=SmoothingMode.AntiAlias;g.InterpolationMode=InterpolationMode.HighQualityBicubic;g.TextRenderingHint=System.Drawing.Text.TextRenderingHint.AntiAliasGridFit;}
 static RectangleF FitRect(float sourceWidth,float sourceHeight,RectangleF destination){float scale=Math.Min(destination.Width/sourceWidth,destination.Height/sourceHeight);float w=sourceWidth*scale,h=sourceHeight*scale;return new RectangleF(destination.X+(destination.Width-w)/2,destination.Y,w,h);}
 // Layout metadata never opens or rasterizes a source page. Rust generates only
 // the source images needed by the requested five-page preview window.
 static List<LogicalPage> Paginate(PrintSource source,int width,int height,string directory){
  var pages=new List<LogicalPage>();
  for(int index=0;index<source.pages.Length;index++){
   var part=source.pages[index];
   var page=new LogicalPage{file=part.image,source_index=index,width=width,height=height,
     image_rect=FitRect(part.width,part.height,new RectangleF(0,0,width,height))};
   foreach(var mark in part.marks){var located=page.Locate(mark);foreach(var rect in mark.rects){var d=page.image_rect;located.rects.Add(new RectangleF(d.X+rect[0]/part.width*d.Width,d.Y+rect[1]/part.height*d.Height,rect[2]/part.width*d.Width,rect[3]/part.height*d.Height));}}
   pages.Add(page);
  }return pages;
 }
 static Color Highlight(string name){switch(name){case "green":return Color.FromArgb(80,70,210,100);case "blue":return Color.FromArgb(80,70,160,240);case "pink":return Color.FromArgb(80,240,100,170);default:return Color.FromArgb(90,255,220,50);}}
 static List<Sheet> Plan(List<LogicalPage> pages,bool notes,int width,int height,Font noteFont){
  var sheets=new List<Sheet>();
  foreach(var page in pages){
   var sheet=new Sheet{page=page};float used=32;
   if(notes)using(var measuring=new Bitmap(1,1))using(var g=Graphics.FromImage(measuring)){
    float lineHeight=noteFont.GetHeight(g)*1.3f;int maxLines=Math.Max(1,(int)((height-64)/lineHeight));
    foreach(var mark in page.marks.Where(m=>m.mark.kind=="note"&&!String.IsNullOrWhiteSpace(m.mark.text))){
     var lines=Wrap(g,mark.mark.text,noteFont,width-16);
     for(int i=0;i<lines.Count;i+=maxLines){var chunk=lines.Skip(i).Take(maxLines).ToList();float h=chunk.Count*lineHeight+20;
      if(used+h>height&&sheet.notes.Count>0){sheets.Add(sheet);sheet=new Sheet{page=page};used=32;}
      sheet.notes.Add(new NoteItem{mark=mark,lines=chunk,Height=h});used+=h;
     }
    }
   }
   sheets.Add(sheet);
  }return sheets;
 }
 public static int[] Range(string range,int total){
  if(String.IsNullOrWhiteSpace(range))return Enumerable.Range(0,total).ToArray();
  var result=new List<int>();foreach(var part in range.Split(',')){var limits=part.Trim().Split('-');int start,end;
   if(!Int32.TryParse(limits[0],out start)||limits.Length>2)throw new Exception("页码格式示例：1-3,5");
   if(limits.Length==1)end=start;else if(!Int32.TryParse(limits[1],out end))throw new Exception("无效结束页码");
   if(start<1||end<start||end>total)throw new Exception("页码超出预览范围");
   for(int page=start-1;page<end;page++)if(!result.Contains(page))result.Add(page);
  }return result.ToArray();
 }
 static void Paper(string name,out int width,out int height){switch(name){case "A5":width=583;height=827;break;case "Letter":width=850;height=1100;break;case "Legal":width=850;height=1400;break;default:width=827;height=1169;break;}}
 public static void Printers(string directory){var printers=new List<string>();try{foreach(string printer in PrinterSettings.InstalledPrinters)printers.Add(printer);var preferred=new PrinterSettings().PrinterName;if(printers.Remove(preferred))printers.Insert(0,preferred);}catch{}Write(Path.Combine(directory,"printers.json"),printers);}
 public static void Render(string job,string settingsFile,string directory,int pageStart,int pageCount){
  var source=Read<PrintSource>(job);var settings=Read<ReaderPrintSettings>(settingsFile);
  int pw,ph;Paper(settings.paper,out pw,out ph);if(settings.landscape){int swap=pw;pw=ph;ph=swap;}
  int width=(int)(pw*1.5),height=(int)(ph*1.5),margin=(int)(settings.margin_mm/25.4*150);
  int contentWidth=width-margin*2,contentHeight=height-margin*2;
  int column=(int)(contentWidth*0.25),gap=24;
  int layoutWidth=source.reflow&&settings.notes?contentWidth-column-gap:contentWidth;
  var pages=Paginate(source,layoutWidth,contentHeight,directory);
  using(var noteFont=new Font("Microsoft YaHei",19,FontStyle.Regular,GraphicsUnit.Pixel)){
   var sheets=Plan(pages,settings.notes,column,contentHeight,noteFont);
   // The range is allowed to be incomplete while typing. Printing validates it strictly.
   int[] selected;try{selected=Range(settings.range,sheets.Count);}catch{selected=Enumerable.Range(0,sheets.Count).ToArray();}
   var names=new List<string>();
   var layout=new List<object>();
   for(int i=0;i<sheets.Count;i++){
    var sheet=sheets[i];int ordinal=Array.IndexOf(selected,i);if(ordinal<0)ordinal=i;
    bool left=settings.first_note_left^(settings.duplex!=0&&ordinal%2==1);
    layout.Add(new { page=i+1, note_left=left, notes=sheet.notes.Count, note_characters=sheet.notes.Sum(n=>n.lines.Sum(line=>line.Length)), content_x=margin+(settings.notes&&left?column+gap:0), content_y=margin, source_page=sheet.page.source_index, reflow=source.reflow, body_width=layoutWidth, body_height=contentHeight });
    string name=String.Format("page-{0:D5}.png",i+1);names.Add(name);
    if(i<pageStart||i>=pageStart+pageCount)continue;
    using(var bitmap=new Bitmap(width,height,PixelFormat.Format32bppArgb))using(var g=Graphics.FromImage(bitmap)){
     Quality(g);g.Clear(Color.White);
     var body=new RectangleF(margin+(settings.notes&&left?column+gap:0),margin,contentWidth-(settings.notes?column+gap:0),contentHeight);
     var destination=source.reflow?body:FitRect(sheet.page.width,sheet.page.height,body);
     float sx=destination.Width/sheet.page.width,sy=destination.Height/sheet.page.height;
     var imageRect=sheet.page.image_rect;
     var imageDestination=new RectangleF(destination.X+imageRect.X*sx,destination.Y+imageRect.Y*sy,imageRect.Width*sx,imageRect.Height*sy);
     using(var content=Image.FromFile(sheet.page.file)){g.DrawImage(content,imageDestination);}
     if(settings.highlights)foreach(var mark in sheet.page.marks)using(var brush=new SolidBrush(Highlight(mark.mark.color)))foreach(var rect in mark.rects){g.FillRectangle(brush,destination.X+rect.X*sx,destination.Y+rect.Y*sy,rect.Width*sx,rect.Height*sy);}
     if(settings.notes){
      float noteX=left?margin:width-margin-column,cursor=margin+32,remaining=sheet.notes.Sum(n=>n.Height);
      using(var heading=new Font("Microsoft YaHei",20,FontStyle.Bold,GraphicsUnit.Pixel)){g.DrawString("笔记",heading,Brushes.DimGray,noteX,margin);}
      foreach(var note in sheet.notes){
       var anchor=note.mark.rects.FirstOrDefault();float anchorX=destination.X+(left?anchor.Left:anchor.Right)*sx,anchorY=destination.Y+(anchor.Top+anchor.Height/2)*sy;
       float y=Math.Min(Math.Max(cursor,anchorY),height-margin-remaining);
       using(var pen=new Pen(Color.FromArgb(160,185,190,198),1.5f)){g.DrawLine(pen,anchorX,anchorY,left?noteX+column:noteX,y+10);}
       float lineY=y;using(var format=Format())foreach(var line in note.lines){g.DrawString(line,noteFont,Brushes.Black,new PointF(noteX+6,lineY),format);lineY+=noteFont.GetHeight(g)*1.3f;}
       cursor=y+note.Height;remaining-=note.Height;
      }
     }
     using(var footer=new Font("Segoe UI",14,FontStyle.Regular,GraphicsUnit.Pixel)){string label=(i+1)+" / "+sheets.Count;g.DrawString(label,footer,Brushes.Gray,new PointF(width/2f-20,height-margin+4));}
     if(!settings.color){using(var gray=new Bitmap(width,height))using(var gg=Graphics.FromImage(gray))using(var attributes=new ImageAttributes()){
      var matrix=new ColorMatrix(new float[][]{new float[]{.299f,.299f,.299f,0,0},new float[]{.587f,.587f,.587f,0,0},new float[]{.114f,.114f,.114f,0,0},new float[]{0,0,0,1,0},new float[]{0,0,0,0,1}});attributes.SetColorMatrix(matrix);gg.DrawImage(bitmap,new Rectangle(0,0,width,height),0,0,width,height,GraphicsUnit.Pixel,attributes);gg.Flush();using(var copy=(Bitmap)gray.Clone()){g.Clear(Color.White);g.DrawImageUnscaled(copy,0,0);}
     }}
     bitmap.SetResolution(150,150);bitmap.Save(Path.Combine(directory,name),ImageFormat.Png);
    }
   }
   Write(Path.Combine(directory,"pages.json"),names);
   Write(Path.Combine(directory,"layout.json"),layout);
  }
  foreach(var page in pages)page.Dispose();
 }
 public static void Print(string settingsFile,string directory){
  var settings=Read<ReaderPrintSettings>(settingsFile);var names=Read<string[]>(Path.Combine(directory,"pages.json"));int[] pages=Range(settings.range,names.Length);
  using(var document=new PrintDocument()){
   document.DocumentName="HayakuReader";document.PrintController=new StandardPrintController();
   document.PrinterSettings.PrinterName=settings.printer;if(!document.PrinterSettings.IsValid)throw new Exception("打印机不可用");
   if(settings.duplex!=0&&!document.PrinterSettings.CanDuplex)throw new Exception("所选打印机不支持自动双面打印，请选择单面或更换打印机。");
   document.PrinterSettings.Copies=(short)settings.copies;document.PrinterSettings.Collate=true;
   document.PrinterSettings.Duplex=settings.duplex==0?Duplex.Simplex:(settings.duplex==1?Duplex.Vertical:Duplex.Horizontal);
   if(!String.IsNullOrEmpty(settings.output)){document.PrinterSettings.PrintToFile=true;document.PrinterSettings.PrintFileName=settings.output;}
   int pw,ph;Paper(settings.paper,out pw,out ph);document.DefaultPageSettings.PaperSize=new PaperSize(settings.paper,pw,ph);
   document.DefaultPageSettings.Landscape=settings.landscape;document.DefaultPageSettings.Color=settings.color;document.DefaultPageSettings.Margins=new Margins(0,0,0,0);
   int current=0;document.PrintPage+=(sender,args)=>{
    using(var image=Image.FromFile(Path.Combine(directory,names[pages[current]]))){
     args.Graphics.TranslateTransform(-args.PageSettings.HardMarginX,-args.PageSettings.HardMarginY);
     args.Graphics.DrawImage(image,new RectangleF(0,0,args.PageBounds.Width,args.PageBounds.Height));
    }current++;args.HasMorePages=current<pages.Length;
   };
   document.Print();
  }
 }
}
'@
Add-Type -TypeDefinition $engine -ReferencedAssemblies System.Drawing, System.Web.Extensions, System.Core
switch ($Mode) {
    'printers' { [ReaderPrintEngine]::Printers($OutputDirectory) }
    'plan' { [ReaderPrintEngine]::Render($Job, $Settings, $OutputDirectory, 0, 0) }
    'render' { [ReaderPrintEngine]::Render($Job, $Settings, $OutputDirectory, $PageStart, $PageCount) }
    'print' { [ReaderPrintEngine]::Print($Settings, $OutputDirectory) }
    default { throw 'Unknown print operation' }
}
