import { NextRequest, NextResponse } from 'next/server'

export async function GET(request: NextRequest) {
  return NextResponse.json([{ id: 1, amount: 100 }])
}

export async function POST(request: NextRequest) {
  const body = await request.json()
  return NextResponse.json({ id: 2, ...body }, { status: 201 })
}
